pub mod commands;
pub mod engines;
pub mod error;
pub mod pipeline;
pub mod presets;
pub mod process;
pub mod project;
pub mod reconstruction;
pub mod splatcam;
pub mod video;
use tauri::{Emitter, Manager};
pub fn run_app() {
    tauri::Builder::default()
        .plugin(tauri_plugin_dialog::init())
        .plugin(tauri_plugin_opener::init())
        .manage(commands::PipelineController::default())
        .invoke_handler(tauri::generate_handler![
            commands::check_engines,
            commands::get_settings,
            commands::probe_and_plan,
            commands::start_pipeline,
            commands::start_splatcam_pipeline,
            commands::cancel_pipeline,
            commands::export_ply,
            commands::get_project_overview,
            commands::set_projects_root,
            commands::set_colmap_backend,
            commands::set_cuda_colmap_flavor,
            commands::set_mapper_ba_mode,
            commands::set_ffmpeg_hw_accel,
            commands::set_brush_training_preset,
            commands::set_training_backend,
    commands::set_gsplat_splat_cap,
    commands::set_photometric_mode,
            commands::inspect_splatcam_import,
            commands::download_colmap_cuda,
            commands::open_project_viewer,
            commands::delete_project,
        ])
        .on_window_event(|window, event| {
            if let tauri::WindowEvent::CloseRequested { api, .. } = event {
                if window.state::<commands::PipelineController>().cancel_for_close() {
                    api.prevent_close();
                    let _ = window.emit("pipeline-event", crate::pipeline::PipelineEvent::mapped(
                        crate::pipeline::PipelineStage::Cancelled, 1.0,
                        "正在终止活动任务；清理完成后请再次关闭窗口。",
                    ));
                }
            }
        })
        .run(tauri::generate_context!())
        .expect("failed to run OOOSplat");
}
