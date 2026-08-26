use std::{
    io::ErrorKind,
    path::{Path, PathBuf},
};

use chrono::Utc;
use uuid::Uuid;

use crate::{
    error::{Result, SplatError},
    pipeline::{ProjectMetadata, ProjectPaths, ProjectStatus},
    presets::Quality,
};

/// Write `value` as JSON to `path` atomically: write to a sibling `.tmp` file and rename it
/// over the destination so a partial write never leaves an invalid settings/project file.
pub async fn atomic_write_json<T: serde::Serialize>(path: &Path, value: &T) -> Result<()> {
    let serialized = serde_json::to_vec_pretty(value)?;
    let tmp = path.with_extension("tmp");
    tokio::fs::write(&tmp, &serialized).await?;
    match tokio::fs::rename(&tmp, path).await {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == ErrorKind::NotFound => {
            if let Some(parent) = path.parent() {
                tokio::fs::create_dir_all(parent).await?;
            }
            tokio::fs::rename(&tmp, path).await?;
            Ok(())
        }
        Err(error) => Err(error.into()),
    }
}

#[derive(Debug, Clone)]
pub struct ProjectManager {
    root: PathBuf,
    diagnostic: bool,
}

impl ProjectManager {
    pub fn with_root(root: PathBuf) -> Self {
        Self {
            root,
            diagnostic: false,
        }
    }

    pub fn for_diagnostics(root: PathBuf) -> Self {
        Self {
            root,
            diagnostic: true,
        }
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    pub async fn validate_root(root: &Path) -> Result<()> {
        if !root.exists() {
            tokio::fs::create_dir_all(root).await?;
        }
        if !root.is_dir() {
            return Err(SplatError::InvalidPath(root.to_path_buf()));
        }
        Ok(())
    }

    pub async fn create(
        &self,
        input: &Path,
        quality: Quality,
    ) -> Result<(ProjectPaths, ProjectMetadata)> {
        Self::validate_root(&self.root).await?;
        let id = Uuid::new_v4();
        let folder_name = if self.diagnostic {
            format!("diag-{id}")
        } else {
            let stem = input
                .file_stem()
                .and_then(|value| value.to_str())
                .unwrap_or("video")
                .to_string();
            let date = Utc::now().format("%Y%m%d");
            format!("{date}_{stem}")
        };
        // Claim the leaf directory atomically before creating descendants.
        // This both preserves existing projects and makes concurrent imports
        // choose the next readable suffix instead of sharing an output folder.
        let project = self.create_unique_project_dir(&folder_name).await?;
        let paths = ProjectPaths {
            frames: project.join("frames"),
            colmap: project.join("work").join("colmap"),
            brush: project.join("work").join("brush"),
            training_input: project.join("work").join("training-input"),
            gsplat: project.join("work").join("gsplat"),
            logs: project.join("logs"),
            project: project.clone(),
            metadata: project.join("project.json"),
            state: project.join("state.json"),
        };
        tokio::fs::create_dir_all(&paths.frames).await?;
        tokio::fs::create_dir_all(&paths.colmap).await?;
        tokio::fs::create_dir_all(&paths.brush).await?;
        tokio::fs::create_dir_all(&paths.gsplat).await?;
        tokio::fs::create_dir_all(&paths.logs).await?;
        let metadata = ProjectMetadata {
            id,
            name: project
                .file_name()
                .and_then(|value| value.to_str())
                .unwrap_or("项目")
                .to_string(),
            source_path: input.to_path_buf(),
            quality,
            status: ProjectStatus::Pending,
            created_at: Utc::now(),
            started_at: Some(Utc::now()),
            completed_at: None,
            duration_ms: None,
            output: None,
            failure_message: None,
            app_id: crate::pipeline::PROJECT_APP_ID.to_string(),
            input_source: crate::pipeline::InputSource::Video,
            splatcam_import: None,
            training_backend: crate::engines::TrainingBackend::Brush,
            brush_training_preset: crate::presets::BrushTrainingPreset::A,
            gsplat_splat_cap: crate::presets::GsplatSplatCap::Auto,
            gsplat_densification_strategy: crate::engines::GsplatDensificationStrategy::Mcmc,
            photometric_mode: crate::engines::PhotometricMode::None,
            timings: crate::pipeline::PipelineTimings::default(),
            colmap_execution: crate::pipeline::ColmapExecution::default(),
            needs_supplement: None,
            supplemental_media: Vec::new(),
        };
        self.write_metadata(&paths.metadata, &metadata).await?;
        Ok((paths, metadata))
    }

    async fn create_unique_project_dir(&self, base_name: &str) -> Result<PathBuf> {
        for index in 1_u32.. {
            let name = if index == 1 {
                base_name.to_string()
            } else {
                format!("{base_name} {index}")
            };
            let candidate = self.root.join(name);
            match tokio::fs::create_dir(&candidate).await {
                Ok(()) => return Ok(candidate),
                Err(error) if error.kind() == ErrorKind::AlreadyExists => continue,
                Err(error) => return Err(error.into()),
            }
        }
        unreachable!("u32 folder suffix range is exhaustive")
    }

    pub async fn write_metadata(&self, path: &Path, metadata: &ProjectMetadata) -> Result<()> {
        atomic_write_json(path, metadata).await
    }

    pub async fn write_state(
        &self,
        path: &Path,
        state: &crate::pipeline::PipelineStateFile,
    ) -> Result<()> {
        atomic_write_json(path, state).await
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn same_video_name_gets_space_separated_increment() {
        let temp = tempfile::tempdir().unwrap();
        let manager = ProjectManager::with_root(temp.path().to_path_buf());
        let input = temp.path().join("drive.mp4");
        let (first, _) = manager.create(&input, Quality::Standard).await.unwrap();
        let (second, _) = manager.create(&input, Quality::Standard).await.unwrap();
        let first_name = first.project.file_name().unwrap().to_string_lossy();
        let second_name = second.project.file_name().unwrap().to_string_lossy();
        assert_eq!(second_name, format!("{first_name} 2"));
        assert!(first.project.is_dir() && second.project.is_dir());
    }
}
