use chaos_ipc::models::ImageDetail;
use chaos_ipc::openai_models::ModelInfo;

pub(crate) fn can_request_original_image_detail(model_info: &ModelInfo) -> bool {
    model_info.supports_image_detail_original
}

#[allow(dead_code)]
pub(crate) fn normalize_output_image_detail(
    model_info: &ModelInfo,
    detail: Option<ImageDetail>,
) -> Option<ImageDetail> {
    match detail {
        Some(ImageDetail::Original) if can_request_original_image_detail(model_info) => {
            Some(ImageDetail::Original)
        }
        Some(ImageDetail::Original) | Some(_) | None => None,
    }
}

#[cfg(test)]
#[path = "original_image_detail_tests.rs"]
mod tests;
