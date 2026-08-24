use crate::pipeline::PipelineStage;

pub fn stage_progress_range(stage: PipelineStage) -> (f32, f32) {
    match stage {
        PipelineStage::ProbingVideo => (0.00, 0.05),
        PipelineStage::PlanningFrames => (0.05, 0.10),
        PipelineStage::ExtractingFrames => (0.10, 0.25),
        PipelineStage::SelectingFrames => (0.25, 0.30),
        PipelineStage::ExtractingFeatures => (0.30, 0.45),
        PipelineStage::Matching => (0.45, 0.55),
        PipelineStage::Reconstructing => (0.55, 0.70),
        PipelineStage::ValidatingReconstruction => (0.70, 0.75),
        PipelineStage::NeedsSupplement => (0.75, 0.75),
        PipelineStage::TrainingSplats => (0.75, 0.97),
        PipelineStage::Exporting => (0.97, 1.00),
        PipelineStage::Completed => (1.00, 1.00),
        PipelineStage::Cancelled => (1.00, 1.00),
        PipelineStage::Failed => (1.00, 1.00),
    }
}
