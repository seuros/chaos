pub struct SessionHeader {
    model: String,
}

impl SessionHeader {
    pub fn new(model: String) -> Self {
        Self { model }
    }

    /// Updates the header's model text.
    pub fn set_model(&mut self, model: &str) {
        if self.model != model {
            self.model = model.to_string();
        }
    }
}
