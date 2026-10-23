use chaos_ipc::openai_models::ModelInfo;

pub(crate) fn can_request_original_image_detail(model_info: &ModelInfo) -> bool {
    model_info.supports_image_detail_original
}

#[cfg(test)]
mod tests;
