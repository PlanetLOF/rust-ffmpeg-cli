//! Batch video encoder built on top of `ffmpeg-next` (libav*) — runs fully
//! in-process (no external `ffmpeg`/`ffprobe` binaries).
//!
//! Per input file it:
//!   1. demuxes with libavformat,
//!   2. decodes the video stream with CUDA hardware acceleration,
//!   3. runs a `scale_cuda` filter graph (format conversion stays on the GPU),
//!   4. encodes with `hevc_nvenc` (constqp @ 22, preset slow, main/main10),
//!   5. stream-copies audio/subtitles (like the old shell-out version did).
//!
//! The hardware parts (CUDA device, hardware frame contexts, filter graph with
//! hardware frames) are not exposed by the safe `ffmpeg-next` API, so they use
//! raw FFI through `ffmpeg::ffi` (a.k.a. `ffmpeg_sys_next`).

use std::ffi::{c_int, CString};
use std::path::{Path, PathBuf};
use std::ptr;
use std::time::Instant;

use anyhow::{anyhow, bail, Context as _, Result};
use clap::Parser;

use ffmpeg_next as ffmpeg;
use ffmpeg::ffi::*;
use ffmpeg::{codec, format, frame, log, media, Dictionary, Error, Packet, Rational};

#[derive(Parser, Debug)]
#[command(
    author,
    version,
    about = "Rust FFmpeg NVENC Batch Encoder (in-process, FFmpeg 8.1 / ffmpeg-next)",
    long_about = None
)]
struct Args {
    /// Input video file paths or folder paths
    #[arg(short, long, required = true, num_args = 1..)]
    inputs: Vec<PathBuf>,

    /// Base destination directory for output files (Defaults to `output` in the current directory)
    #[arg(short, long)]
    output_dir: Option<PathBuf>,

    /// Custom output file extension (e.g., mkv, mp4)
    #[arg(short, long, default_value = "mkv")]
    extension: String,

    /// Custom suffix to append to the input file name (e.g., '_enc')
    #[arg(short, long, default_value = "_enc")]
    suffix: String,
}

fn main() -> Result<()> {
    let args = Args::parse();

    ffmpeg::init().context("failed to initialize FFmpeg")?;
    log::set_level(log::Level::Warning);

    // 1. Discover all actual video files from the provided inputs (files or folders)
    let mut files_to_process = Vec::new();
    for input_path in args.inputs {
        collect_video_files(input_path, &mut files_to_process);
    }

    if files_to_process.is_empty() {
        eprintln!("⚠️ No valid video files found to process.");
        return Ok(());
    }

    println!(
        "🚀 Starting batch processing of {} files...",
        files_to_process.len()
    );

    // 2. Process the gathered files
    for input_path in files_to_process {
        // Determine the output directory dynamically:
        let base_dir = match &args.output_dir {
            Some(dir) => dir.clone(),
            None => PathBuf::from("output"),
        };
        let parent_name = input_path
            .parent()
            .and_then(|p| p.file_name())
            .unwrap_or(Path::new(".").as_os_str());
        let target_dir = base_dir.join(parent_name);

        if !target_dir.exists() {
            std::fs::create_dir_all(&target_dir)?;
        }

        let output_path = build_output_path(&input_path, &target_dir, &args.suffix, &args.extension);

        println!("\n🎬 Processing: {:?}", input_path);
        match process_video(&input_path, &output_path) {
            Ok(()) => println!("✅ Successfully encoded: {:?}", output_path),
            Err(e) => eprintln!("❌ Failed to encode {:?}:\n   {:#}", input_path, e),
        }
    }

    println!("\n🎉 All processes completed!");
    Ok(())
}

/// Encodes a single input video into `output_path`.
fn process_video(input: &Path, output: &Path) -> Result<()> {
    let mut ictx = format::input(input)
        .with_context(|| format!("could not open input `{}`", input.display()))?;

    let video_stream = ictx
        .streams()
        .best(media::Type::Video)
        .ok_or_else(|| anyhow!("no video stream found"))?;
    let vindex = video_stream.index();
    let video_stream_tb = video_stream.time_base();

    // Detect the source bit depth and pick the scale_cuda output format and
    // nvenc profile accordingly (avoids the broken 8->10 bit conversion in
    // scale_cuda that produced green frames with NVENC).
    let src_pix_name = source_pixel_format_name(&video_stream);
    let is_10bit = is_10bit_pix_fmt(&src_pix_name);
    let scale_format = if is_10bit { "p010le" } else { "nv12" };
    let profile = if is_10bit { "main10" } else { "main" };
    let width = unsafe { (*video_stream.parameters().as_ptr()).width };
    let height = unsafe { (*video_stream.parameters().as_ptr()).height };

    println!(
        "   Source pixel format: {} ({} bit) | scale_cuda → {} | profile: {}",
        src_pix_name,
        if is_10bit { "10" } else { "8" },
        scale_format,
        profile
    );
    println!("   Saving to: {:?}", output);

    // 3. Set up the muxer and all output streams.
    // NOTE: the video output stream is reserved here (to keep the stream
    // index mapping stable) but its codec parameters are only filled in once
    // the first frame has been decoded (`VideoPipeline::prepare`) — the
    // decoder's hardware frames context (which the filter graph needs) only
    // appears at that point.
    let mut octx = format::output(output)
        .with_context(|| format!("could not create output `{}`", output.display()))?;
    octx.set_metadata(ictx.metadata().to_owned());

    let global_header = octx.format().flags().contains(format::Flags::GLOBAL_HEADER);

    let mut stream_mapping: Vec<isize> = vec![0; ictx.nb_streams() as usize];
    let mut ist_time_bases = vec![Rational(0, 0); ictx.nb_streams() as usize];
    let mut ost_time_bases = vec![Rational(0, 0); ictx.nb_streams() as usize];

    let mut pipeline: Option<VideoPipeline> = None;
    let mut ost_index = 0usize;

    for (ist_index, ist) in ictx.streams().enumerate() {
        let ist_medium = ist.parameters().medium();
        if ist_medium != media::Type::Audio
            && ist_medium != media::Type::Video
            && ist_medium != media::Type::Subtitle
        {
            stream_mapping[ist_index] = -1;
            continue;
        }

        stream_mapping[ist_index] = ost_index as isize;
        ist_time_bases[ist_index] = ist.time_base();

        if ist_index == vindex {
            // Re-encode the (best) video stream with NVENC.
            let mut pipe = VideoPipeline::new(&ist, ost_index, width, height, &src_pix_name)?;
            pipe.input_time_base = video_stream_tb;
            pipeline = Some(pipe);

            // Reserve the output slot now; `prepare()` fills it in.
            let mut v_ost = octx.add_stream(ffmpeg::encoder::find_by_name("hevc_nvenc"))?;
            unsafe {
                (*(*v_ost.as_mut_ptr()).codecpar).codec_tag = 0;
            }
        } else {
            // Stream-copy every non-video stream (and any extra video streams).
            let mut ost = octx.add_stream(codec::encoder::find(codec::Id::None))?;
            ost.set_parameters(ist.parameters());
            // Ensure the stream can be copied into the chosen container.
            unsafe {
                (*(*ost.as_mut_ptr()).codecpar).codec_tag = 0;
            }
        }

        ost_index += 1;
    }

    // 4. Pump packets: re-encode video, copy the rest.
    // The output header can only be written once the encoder stream exists,
    // which happens on the first decoded video frame. Until then, copy-stream
    // packets are held in `prelude`.
    let mut primed = false;
    let mut header_written = false;
    let mut prelude: Vec<(usize, Packet)> = Vec::new();

    for (stream, packet) in ictx.packets() {
        let ist_index = stream.index();
        let ost_index = stream_mapping[ist_index];
        if ost_index < 0 {
            continue;
        }

        if ist_index == vindex {
            let pipe = pipeline
                .as_mut()
                .expect("video pipeline not initialized");
            if !primed {
                pipe.decoder.send_packet(&packet)?;
                let mut first = frame::Video::empty();
                match pipe.decoder.receive_frame(&mut first) {
                    Ok(()) => {
                        primed = true;
                        ensure_cuda_frame(&first)?;
                        pipe.prepare(&mut octx, global_header)?;
                        write_header_and_replay(
                            &mut octx,
                            &mut header_written,
                            &mut prelude,
                            &stream_mapping,
                            &ist_time_bases,
                            &mut ost_time_bases,
                        )?;
                        let v_ost_time_base = ost_time_bases[pipe.ost_index];
                        pipe.push_frame(&mut octx, &mut first, v_ost_time_base)?;
                    }
                    Err(Error::Eof) => { /* tail frames handled at flush */ }
                    Err(Error::Other { errno: EAGAIN }) => { /* need more packets */ }
                    Err(e) => bail!("decode error: {e}"),
                }
            } else {
                pipe.decoder.send_packet(&packet)?;
                pipe.drain_decoded_frames(&mut octx, ost_time_bases[pipe.ost_index])?;
            }
        } else {
            // Stream-copy packets: rescale later, once the muxer header has set
            // the real output time bases.
            if header_written {
                let ost_time_base = ost_time_bases[ost_index as usize];
                write_copied_packet(
                    &mut octx,
                    packet,
                    ist_time_bases[ist_index],
                    ost_time_base,
                    ost_index as usize,
                )?;
            } else {
                prelude.push((ist_index, packet));
            }
        }
    }

    // 5. Flush decoder, filter graph and encoder.
    if let Some(pipe) = pipeline.as_mut() {
        if !primed {
            // Try to coax a first frame out of the decoder's tail buffer; a
            // decoder may only start emitting frames once it hits EOF.
            pipe.decoder.send_eof()?;
            let mut first = frame::Video::empty();
            match pipe.decoder.receive_frame(&mut first) {
                Ok(()) => {
                    ensure_cuda_frame(&first)?;
                    pipe.prepare(&mut octx, global_header)?;
                    write_header_and_replay(
                        &mut octx,
                        &mut header_written,
                        &mut prelude,
                        &stream_mapping,
                        &ist_time_bases,
                        &mut ost_time_bases,
                    )?;
                    let v_ost_time_base = ost_time_bases[pipe.ost_index];
                    pipe.push_frame(&mut octx, &mut first, v_ost_time_base)?;
                    pipe.drain_decoded_frames(&mut octx, v_ost_time_base)?;
                    pipe.finish_flush(&mut octx, v_ost_time_base)?;
                }
                Err(_) => {
                    // The video stream produced no frames at all; the copied
                    // streams still need a header + trailer below.
                }
            }
        } else {
            let v_ost_time_base = ost_time_bases[pipe.ost_index];
            pipe.flush(&mut octx, v_ost_time_base)?;
        }
    }

    write_header_and_replay(
        &mut octx,
        &mut header_written,
        &mut prelude,
        &stream_mapping,
        &ist_time_bases,
        &mut ost_time_bases,
    )?;
    octx.write_trailer()?;
    Ok(())
}

/// Writes the muxer header (once), records the output streams' time bases
/// (only valid after the header) and replays any copy-stream packets that
/// arrived before the video pipeline was ready.
fn write_header_and_replay(
    octx: &mut format::context::Output,
    header_written: &mut bool,
    prelude: &mut Vec<(usize, Packet)>,
    stream_mapping: &[isize],
    ist_time_bases: &[Rational],
    ost_time_bases: &mut [Rational],
) -> Result<()> {
    if *header_written {
        return Ok(());
    }

    octx.write_header()?;
    for (idx, tb) in ost_time_bases.iter_mut().enumerate() {
        *tb = octx
            .stream(idx)
            .map(|s| s.time_base())
            .unwrap_or(Rational(0, 0));
    }

    for (ist_index, packet) in prelude.drain(..) {
        let ost_index = stream_mapping[ist_index];
        if ost_index < 0 {
            continue;
        }
        write_copied_packet(
            octx,
            packet,
            ist_time_bases[ist_index],
            ost_time_bases[ost_index as usize],
            ost_index as usize,
        )?;
    }
    prelude.clear();

    *header_written = true;
    Ok(())
}

/// Writes a stream-copied packet to the muxer (rescaling + re-tagging it for
/// the output stream).
fn write_copied_packet(
    octx: &mut format::context::Output,
    mut packet: Packet,
    ist_time_base: Rational,
    ost_time_base: Rational,
    ost_index: usize,
) -> Result<()> {
    packet.rescale_ts(ist_time_base, ost_time_base);
    packet.set_position(-1);
    packet.set_stream(ost_index);
    packet.write_interleaved(octx)?;
    Ok(())
}

/// Everything needed to transcode one video stream through the GPU pipeline.
struct VideoPipeline {
    decoder: codec::decoder::Video,
    /// `None` until `prepare()` has run (i.e. until the first decoded frame).
    encoder: Option<codec::encoder::Video>,
    /// `AVFilterGraph*` — owns `src`/`sink` filter contexts.
    graph: *mut AVFilterGraph,
    /// `AVFilterContext*` for the `buffer` (source) filter.
    src: *mut AVFilterContext,
    /// `AVFilterContext*` for the `buffersink` filter.
    sink: *mut AVFilterContext,
    /// `AVBufferRef*` for the CUDA hw device.
    device: *mut AVBufferRef,

    scale_format: String,
    profile: String,
    width: i32,
    height: i32,
    input_time_base: Rational,

    ost_index: usize,
    prepared: bool,

    frame_count: usize,
    last_log: Instant,
}

impl VideoPipeline {
    /// Creates the CUDA-accelerated decoder. The filter graph and encoder are
    /// deliberately NOT set up yet — the decoder only allocates its hardware
    /// frames context once the first frame is decoded, and the graph needs
    /// that pool. Call `prepare()` once you hold the first frame.
    fn new(
        ist: &format::stream::Stream,
        ost_index: usize,
        width: i32,
        height: i32,
        src_pix_name: &str,
    ) -> Result<Self> {
        let is_10bit = is_10bit_pix_fmt(src_pix_name);
        let scale_format = if is_10bit { "p010le" } else { "nv12" };
        let profile = if is_10bit { "main10" } else { "main" };

        // ---- CUDA hardware device + hwaccel decoder -----------------------
        let mut device: *mut AVBufferRef = ptr::null_mut();
        let ret = unsafe {
            av_hwdevice_ctx_create(
                &mut device,
                AVHWDeviceType::AV_HWDEVICE_TYPE_CUDA,
                ptr::null(),
                ptr::null_mut(),
                0,
            )
        };
        if ret < 0 || device.is_null() {
            bail!(
                "could not create CUDA hw device (error {ret}): is a CUDA-capable \
                 NVIDIA GPU with current drivers available?"
            );
        }

        let mut decoder =
            codec::context::Context::from_parameters(ist.parameters())?.decoder();

        unsafe {
            let avctx = decoder.as_mut_ptr();
            (*avctx).hw_device_ctx = av_buffer_ref(device);
            (*avctx).get_format = Some(cuda_get_format);
        }

        let video = decoder.video()?;

        Ok(Self {
            decoder: video,
            encoder: None,
            graph: ptr::null_mut(),
            src: ptr::null_mut(),
            sink: ptr::null_mut(),
            device,
            scale_format: scale_format.to_string(),
            profile: profile.to_string(),
            width,
            height,
            input_time_base: ist.time_base(),
            ost_index,
            prepared: false,
            frame_count: 0,
            last_log: Instant::now(),
        })
    }

    /// Builds the `buffer → scale_cuda → buffersink` graph and the `hevc_nvenc`
    /// encoder once the decoder has produced its first (hardware) frame, then
    /// fills in the output stream that `process_video` reserved.
    fn prepare(&mut self, octx: &mut format::context::Output, global_header: bool) -> Result<()> {
        assert!(!self.prepared, "prepare() called twice");

        // The decoder allocated its hardware frames context when the first
        // frame was decoded; the filter source must use that same pool.
        let dec_hw_frames: *mut AVBufferRef = unsafe { (*self.decoder.as_mut_ptr()).hw_frames_ctx };
        if dec_hw_frames.is_null() {
            bail!("decoder did not create a hardware frame context (cuvid missing?)");
        }

        let mut graph = unsafe { avfilter_graph_alloc() };
        if graph.is_null() {
            unsafe {
                av_buffer_unref(&mut self.device);
            }
            bail!("out of memory allocating filter graph");
        }

        let src: *mut AVFilterContext;
        let mut scale: *mut AVFilterContext = ptr::null_mut();
        let mut sink: *mut AVFilterContext = ptr::null_mut();

        unsafe {
            let buffer_f = avfilter_get_by_name(c"buffer".as_ptr());
            let scale_cuda_f = avfilter_get_by_name(c"scale_cuda".as_ptr());
            let buffersink_f = avfilter_get_by_name(c"buffersink".as_ptr());
            if buffer_f.is_null() || scale_cuda_f.is_null() || buffersink_f.is_null() {
                avfilter_graph_free(&mut graph);
                av_buffer_unref(&mut self.device);
                bail!(
                    "this FFmpeg build lacks one of the required filters \
                     (buffer / scale_cuda / buffersink)"
                );
            }

            // ---- buffer source ---------------------------------------------
            // `avfilter_graph_create_filter` initializes the filter, and the
            // buffer filter's init rejects HW pixel formats without a hardware
            // frames context and rejects missing dimensions/time base — so the
            // options must be in place *before* init. Do that by hand:
            // allocate (uninitialized), set AVBufferSrcParameters, then init.
            src = avfilter_graph_alloc_filter(graph, buffer_f, c"in".as_ptr());
            if src.is_null() {
                avfilter_graph_free(&mut graph);
                av_buffer_unref(&mut self.device);
                bail!("out of memory allocating the `buffer` filter");
            }

            let params = av_buffersrc_parameters_alloc();
            if params.is_null() {
                avfilter_graph_free(&mut graph);
                av_buffer_unref(&mut self.device);
                bail!("out of memory allocating buffer source parameters");
            }
            (*params).format = AVPixelFormat::AV_PIX_FMT_CUDA as c_int;
            (*params).time_base = self.input_time_base.into();
            (*params).width = self.width;
            (*params).height = self.height;

            let sar = self.decoder.aspect_ratio();
            if sar.numerator() > 0 && sar.denominator() > 0 {
                (*params).sample_aspect_ratio = sar.into();
            }
            if let Some(framerate) = self.decoder.frame_rate() {
                (*params).frame_rate = framerate.into();
            }

            // The decoded hardware frames belong to `dec_hw_frames`; hand that
            // frame context to the buffersrc so frames flow GPU → GPU.
            (*params).hw_frames_ctx = dec_hw_frames;

            // Match the colorspace/range the decoder derived from the stream
            // headers (av_buffersrc_parameters_set ignores UNSPECIFIED values,
            // in which case the graph negotiates defaults downstream).
            (*params).color_space = (*self.decoder.as_ptr()).colorspace;
            (*params).color_range = (*self.decoder.as_ptr()).color_range;

            let ret = av_buffersrc_parameters_set(src, params);
            av_free(params as *mut std::os::raw::c_void);
            if ret < 0 {
                avfilter_graph_free(&mut graph);
                av_buffer_unref(&mut self.device);
                bail!("av_buffersrc_parameters_set failed (error {ret})");
            }

            let ret = avfilter_init_dict(src, ptr::null_mut());
            if ret < 0 {
                avfilter_graph_free(&mut graph);
                av_buffer_unref(&mut self.device);
                bail!("buffer filter initialization failed (error {ret})");
            }

            let scale_args = CString::new(format!("format={}", self.scale_format)).unwrap();
            check_graph_filter_alloc(
                avfilter_graph_create_filter(
                    &mut scale,
                    scale_cuda_f,
                    c"scale_cuda".as_ptr(),
                    scale_args.as_ptr(),
                    ptr::null_mut(),
                    graph,
                ),
                "scale_cuda",
                &mut graph,
                &mut self.device,
            )?;
            check_graph_filter_alloc(
                avfilter_graph_create_filter(
                    &mut sink,
                    buffersink_f,
                    c"out".as_ptr(),
                    ptr::null(),
                    ptr::null_mut(),
                    graph,
                ),
                "buffersink",
                &mut graph,
                &mut self.device,
            )?;

            avfilter_link(src, 0, scale, 0);
            avfilter_link(scale, 0, sink, 0);

            let ret = avfilter_graph_config(graph, ptr::null_mut());
            if ret < 0 {
                avfilter_graph_free(&mut graph);
                av_buffer_unref(&mut self.device);
                bail!("filter graph configuration failed (error {ret})");
            }

            let sink_hw_frames = av_buffersink_get_hw_frames_ctx(sink);
            if sink_hw_frames.is_null() {
                avfilter_graph_free(&mut graph);
                av_buffer_unref(&mut self.device);
                bail!("scale_cuda did not produce a hardware frame context");
            }

            // ---- hevc_nvenc encoder fed by the filter sink's hw frames -----
            let encoder_codec = ffmpeg::encoder::find_by_name("hevc_nvenc")
                .ok_or_else(|| anyhow!("hevc_nvenc encoder not found in this FFmpeg build"))?;

            let mut encoder =
                codec::context::Context::new_with_codec(encoder_codec).encoder().video()?;

            encoder.set_width(self.decoder.width());
            encoder.set_height(self.decoder.height());
            encoder.set_format(ffmpeg::util::format::Pixel::CUDA);
            encoder.set_aspect_ratio(self.decoder.aspect_ratio());
            if let Some(framerate) = self.decoder.frame_rate() {
                encoder.set_frame_rate(Some(framerate));
            } else {
                // Fall back to the inverse of the stream time base.
                let tb = self.input_time_base;
                encoder.set_frame_rate(Some(Rational(tb.denominator(), tb.numerator())));
            }
            encoder.set_time_base(self.input_time_base);

            if global_header {
                encoder.set_flags(codec::Flags::GLOBAL_HEADER);
            }

            (*encoder.as_mut_ptr()).hw_frames_ctx = av_buffer_ref(sink_hw_frames);

            let mut opts = Dictionary::new();
            opts.set("preset", "slow");
            opts.set("rc", "constqp");
            opts.set("cq", "22");
            opts.set("profile", self.profile.as_str());

            let opened_encoder = encoder
                .open_with(opts)
                .context("failed to open hevc_nvenc with the requested options")?;

            {
                let mut ost = octx
                    .stream_mut(self.ost_index)
                    .ok_or_else(|| anyhow!("video output stream disappeared"))?;
                ost.set_parameters(&opened_encoder);
            }

            self.graph = graph;
            self.src = src;
            self.sink = sink;
            self.encoder = Some(opened_encoder);
        }

        self.prepared = true;
        Ok(())
    }

    /// Feeds one decoded frame into the filter graph and drains whatever
    /// frames/graph output become available.
    fn push_frame(
        &mut self,
        octx: &mut format::context::Output,
        frame: &mut frame::Video,
        ost_time_base: Rational,
    ) -> Result<()> {
        let ret = unsafe { av_buffersrc_add_frame(self.src, frame.as_mut_ptr()) };
        if ret < 0 {
            bail!("av_buffersrc_add_frame failed (error {ret})");
        }
        self.drain_filter_to_encoder(octx, ost_time_base)
    }

    /// Drains every frame the decoder currently has buffered into the filter
    /// graph, and pipes whatever the graph outputs into the encoder/muxer.
    fn drain_decoded_frames(
        &mut self,
        octx: &mut format::context::Output,
        ost_time_base: Rational,
    ) -> Result<()> {
        let mut frame = frame::Video::empty();
        while self.decoder.receive_frame(&mut frame).is_ok() {
            self.frame_count += 1;
            let ret = unsafe { av_buffersrc_add_frame(self.src, frame.as_mut_ptr()) };
            if ret < 0 {
                bail!("av_buffersrc_add_frame failed (error {ret})");
            }
            self.drain_filter_to_encoder(octx, ost_time_base)?;
            self.log_progress();
        }
        Ok(())
    }

    /// Flushes the decoder, filter graph and encoder at end of stream.
    fn flush(
        &mut self,
        octx: &mut format::context::Output,
        ost_time_base: Rational,
    ) -> Result<()> {
        self.decoder.send_eof()?;
        self.drain_decoded_frames(octx, ost_time_base)?;
        self.finish_flush(octx, ost_time_base)
    }

    /// Closes the filter graph and flushes the encoder (decoder already done).
    fn finish_flush(
        &mut self,
        octx: &mut format::context::Output,
        ost_time_base: Rational,
    ) -> Result<()> {
        // Signal EOF to the filter graph, then drain whatever it still yields
        // (including the last frame(s) held back by the encoder delay).
        unsafe {
            av_buffersrc_close(self.src, i64::MIN, 0);
        }
        self.drain_filter_to_encoder(octx, ost_time_base)?;

        self.encoder_mut().send_eof()?;
        self.receive_encoded_packets(octx, ost_time_base)
    }

    /// Pulls frames out of the buffersink and feeds them to the encoder.
    fn drain_filter_to_encoder(
        &mut self,
        octx: &mut format::context::Output,
        ost_time_base: Rational,
    ) -> Result<()> {
        let mut out = frame::Video::empty();
        loop {
            let ret = unsafe { av_buffersink_get_frame(self.sink, out.as_mut_ptr()) };
            if ret < 0 {
                break; // EAGAIN (need more input) or EOF
            }
            self.encoder_mut().send_frame(&out)?;
            self.receive_encoded_packets(octx, ost_time_base)?;
        }
        Ok(())
    }

    fn receive_encoded_packets(
        &mut self,
        octx: &mut format::context::Output,
        ost_time_base: Rational,
    ) -> Result<()> {
        let mut encoded = Packet::empty();
        while self.encoder_mut().receive_packet(&mut encoded).is_ok() {
            encoded.set_stream(self.ost_index);
            encoded.rescale_ts(self.input_time_base, ost_time_base);
            encoded.write_interleaved(octx)?;
        }
        Ok(())
    }

    fn encoder_mut(&mut self) -> &mut codec::encoder::Video {
        self.encoder.as_mut().expect("encoder not prepared")
    }

    fn log_progress(&mut self) {
        if self.last_log.elapsed().as_secs_f64() >= 1.0 {
            eprintln!("   ... decoded {} frames", self.frame_count);
            self.last_log = Instant::now();
        }
    }
}

impl Drop for VideoPipeline {
    fn drop(&mut self) {
        unsafe {
            if !self.graph.is_null() {
                avfilter_graph_free(&mut self.graph);
            }
            if !self.device.is_null() {
                av_buffer_unref(&mut self.device);
            }
            // The decoder (with its hw_frames_ctx / hw_device_ctx refs) and the
            // encoder are owned by their Rust wrappers and freed afterwards.
        }
    }
}

/// `AVCodecContext::get_format` callback that redirects the decoder to CUDA
/// hardware frames whenever the decoder supports them.
unsafe extern "C" fn cuda_get_format(
    _avctx: *mut AVCodecContext,
    fmt: *const AVPixelFormat,
) -> AVPixelFormat {
    unsafe {
        if fmt.is_null() {
            return AVPixelFormat::AV_PIX_FMT_NONE;
        }

        let mut p = fmt;
        loop {
            let current = *p;
            if current == AVPixelFormat::AV_PIX_FMT_NONE {
                break;
            }
            if current == AVPixelFormat::AV_PIX_FMT_CUDA {
                return current;
            }
            p = p.add(1);
        }

        // No CUDA support in the decoder's format list — fall back to whatever
        // it prefers (caller checks for CUDA afterwards and reports a clean error).
        *fmt
    }
}

/// Turns the return code of `avfilter_graph_create_filter` into a `Result`,
/// releasing the partially-built filter graph and the CUDA device on failure.
fn check_graph_filter_alloc(
    ret: c_int,
    name: &str,
    graph: &mut *mut AVFilterGraph,
    device: &mut *mut AVBufferRef,
) -> Result<()> {
    if ret < 0 {
        unsafe {
            avfilter_graph_free(graph);
            av_buffer_unref(device);
        }
        bail!("failed to create `{name}` filter (error {ret})");
    }
    Ok(())
}

/// Verifies that a decoded frame really is a CUDA hardware frame, with a
/// helpful error if the CUDA path did not engage.
fn ensure_cuda_frame(frame: &frame::Video) -> Result<()> {
    if frame.format() != ffmpeg::util::format::Pixel::CUDA {
        bail!(
            "CUDA hardware decoding is not available for this stream \
             (decoder produced {:?} instead of cuda)",
            frame.format().descriptor().map(|d| d.name())
        );
    }
    Ok(())
}

/// Reads the pixel format name of the first video stream from its codec
/// parameters (populated by `avformat_find_stream_info`).
fn source_pixel_format_name(stream: &format::stream::Stream) -> String {
    unsafe {
        let raw_format = (*stream.parameters().as_ptr()).format;
        if raw_format < 0 {
            return "unknown".to_string();
        }
        // The bindings expose AVPixelFormat as a Rust enum; the underlying
        // representation is exactly the C `int` value, so this transmute is
        // the standard FFI bridge (the crate itself does the same).
        let pix_fmt = std::mem::transmute::<i32, AVPixelFormat>(raw_format);
        let name = av_get_pix_fmt_name(pix_fmt);
        if name.is_null() {
            "unknown".to_string()
        } else {
            std::ffi::CStr::from_ptr(name).to_string_lossy().into_owned()
        }
    }
}

/// Returns true for 10-bit pixel formats (e.g. yuv420p10le, p010le)
fn is_10bit_pix_fmt(pix_fmt: &str) -> bool {
    let p = pix_fmt.to_ascii_lowercase();
    p.contains("p10") || p.contains("10le") || p.contains("10be")
}

/// Helper function to construct the custom output file path
fn build_output_path(input: &Path, out_dir: &Path, suffix: &str, ext: &str) -> PathBuf {
    let stem = input.file_stem().unwrap().to_string_lossy();
    let new_filename = format!("{stem}{suffix}.{ext}");
    out_dir.join(new_filename)
}

/// Recursively scans directories for files matching common video extensions
fn collect_video_files(path: PathBuf, files: &mut Vec<PathBuf>) {
    // List of standard video extensions to look for
    let valid_extensions = ["mp4", "mkv", "mov", "avi", "flv", "webm", "m4v", "wmv"];

    if path.is_file() {
        if let Some(ext) = path.extension().and_then(|e| e.to_str())
            && valid_extensions.contains(&ext.to_lowercase().as_str())
        {
            files.push(path);
        }
    } else if path.is_dir()
        && let Ok(entries) = std::fs::read_dir(path)
    {
        for entry in entries.flatten() {
            collect_video_files(entry.path(), files);
        }
    }
}