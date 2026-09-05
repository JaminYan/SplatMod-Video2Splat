use std::path::PathBuf;

use clap::{Parser, Subcommand};

use ooo_splat::{
    engines::{
        colmap::convert_text_model_to_binary, ffmpeg::extract_uniform_frames, ffprobe::probe_video,
        FfmpegHwAccel,
    },
    error::{Result, SplatError},
    pipeline::runner::{default_engine_paths, PipelineRunner},
    presets::Quality,
    process::ProcessManager,
    video::{FrameSelectionStrategy, UniformRatioFrameSelection},
};

#[derive(Debug, Parser)]
#[command(name = "splatstudio", version, about = "OOOSplat local pipeline CLI")]
struct Cli {
    /// Override the bundled engine directory (also supports OOOSPLAT_ENGINE_DIR).
    #[arg(long, global = true)]
    engine_dir: Option<PathBuf>,

    #[command(subcommand)]
    command: Commands,
}

#[derive(Debug, Subcommand)]
enum Commands {
    /// Validate FFmpeg, FFprobe, CPU COLMAP and Brush.
    Health,
    /// Read video metadata through FFprobe JSON.
    Probe { input: PathBuf },
    /// Show the uniform frame plan without extracting images.
    Plan {
        input: PathBuf,
        #[arg(long, value_enum, default_value_t = Quality::Standard)]
        quality: Quality,
    },
    /// Extract uniformly sampled JPEGs with FFmpeg.
    Extract {
        input: PathBuf,
        output: PathBuf,
        #[arg(long, value_enum, default_value_t = Quality::Standard)]
        quality: Quality,
    },
    /// Validate a Splatcam RGB + COLMAP text + RGB point-cloud export without modifying it.
    SplatcamInspect { input: PathBuf },
    /// Normalize Splatcam into COLMAP text and binary models without running SfM.
    SplatcamNormalize { input: PathBuf, output: PathBuf },
    /// Run the end-to-end pipeline after all fixed engine CLIs are verified.
    Generate {
        input: PathBuf,
        /// Override the remembered projects root (useful for diagnostics).
        #[arg(long)]
        projects_root: Option<PathBuf>,
        #[arg(long, value_enum, default_value_t = Quality::Standard)]
        quality: Quality,
    },
}

#[tokio::main]
async fn main() {
    tracing_subscriber::fmt()
        .with_env_filter(tracing_subscriber::EnvFilter::from_default_env())
        .with_target(false)
        .init();

    if let Err(error) = execute(Cli::parse()).await {
        eprintln!("{error}");
        std::process::exit(1);
    }
}

async fn execute(cli: Cli) -> Result<()> {
    let engines = default_engine_paths(cli.engine_dir);
    match cli.command {
        Commands::Health => {
            println!(
                "{}",
                serde_json::to_string_pretty(&engines.check_all().await)?
            );
        }
        Commands::Probe { input } => {
            let video = probe_video(&engines.ffprobe, &input, None).await?;
            println!("{}", serde_json::to_string_pretty(&video)?);
        }
        Commands::Plan { input, quality } => {
            let video = probe_video(&engines.ffprobe, &input, None).await?;
            let plan = UniformRatioFrameSelection.create_plan(&video, &quality.preset());
            println!("{}", serde_json::to_string_pretty(&plan)?);
        }
        Commands::Extract {
            input,
            output,
            quality,
        } => {
            ensure_engine(&engines.ffprobe)?;
            ensure_engine(&engines.ffmpeg)?;
            let video = probe_video(&engines.ffprobe, &input, None).await?;
            let plan = UniformRatioFrameSelection.create_plan(&video, &quality.preset());
            let count = extract_uniform_frames(
                &engines.ffmpeg,
                &input,
                &output,
                &plan,
                FfmpegHwAccel::Off,
                None,
                &ProcessManager::new(),
                None,
            )
            .await?;
            println!("extracted {count} frames to {}", output.display());
        }
        Commands::SplatcamInspect { input } => {
            let source = input.clone();
            let report =
                tokio::task::spawn_blocking(move || ooo_splat::splatcam::inspect_export(&source))
                    .await
                    .map_err(|error| {
                        SplatError::Process(format!("Splatcam 导入检查任务失败：{error}"))
                    })??;
            println!("{}", serde_json::to_string_pretty(&report)?);
        }
        Commands::SplatcamNormalize { input, output } => {
            if output.exists() {
                return Err(SplatError::Process(format!(
                    "输出目录已存在，拒绝覆盖：{}",
                    output.display()
                )));
            }
            std::fs::create_dir_all(&output)?;
            let text_model = output.join("normalized-model");
            let source = input.clone();
            let text_destination = text_model.clone();
            let report = tokio::task::spawn_blocking(move || {
                let report = ooo_splat::splatcam::inspect_export(&source)?;
                ooo_splat::splatcam::prepare_normalized_text_model(
                    &source,
                    &text_destination,
                    &report,
                )
            })
            .await
            .map_err(|error| SplatError::Process(format!("Splatcam 标准化任务失败：{error}")))??;
            let binary_model = output.join("binary-model");
            std::fs::create_dir(&binary_model)?;
            convert_text_model_to_binary(
                &engines.colmap,
                &text_model,
                &binary_model,
                output.join("model-converter.log"),
                &ProcessManager::new(),
                None,
            )
            .await?;
            ooo_splat::splatcam::verify_binary_model_counts(
                &binary_model,
                report.camera_count,
                report.pose_count,
                report.point_count,
            )?;
            println!("{}", serde_json::to_string_pretty(&report)?);
            println!("normalized model: {}", binary_model.display());
        }
        Commands::Generate {
            input,
            projects_root,
            quality,
        } => {
            let runner = PipelineRunner::new(
                engines,
                ooo_splat::engines::ColmapBackend::Cpu,
                ooo_splat::engines::CudaColmapFlavor::Official,
                ooo_splat::engines::MapperBaMode::Auto,
                FfmpegHwAccel::Off,
                ooo_splat::presets::BrushTrainingPreset::A,
                ooo_splat::presets::GsplatSplatCap::Auto,
                ooo_splat::engines::GsplatDensificationStrategy::Mcmc,
                false,
                false,
                ooo_splat::engines::PhotometricMode::None,
                ooo_splat::engines::TrainingBackend::Brush,
                true,
                |event| {
                    eprintln!(
                        "{:>6.2}% {:?}: {}",
                        event.progress, event.stage, event.message
                    );
                },
            );
            let result = match projects_root {
                Some(root) => {
                    runner
                        .generate_for_diagnostics(&input, quality, &root)
                        .await?
                }
                None => {
                    let root = ooo_splat::project::catalog::load_settings()
                        .await?
                        .projects_root;
                    runner.generate(&input, quality, &root).await?
                }
            };
            println!("{}", serde_json::to_string_pretty(&result)?);
        }
    }
    Ok(())
}

fn ensure_engine(path: &std::path::Path) -> Result<()> {
    if path.is_file() {
        Ok(())
    } else {
        Err(SplatError::EngineMissing(path.display().to_string()))
    }
}
