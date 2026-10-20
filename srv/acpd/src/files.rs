use std::io;
use std::path::Path;

use acp_host::{ClientHandle, SessionContext};
use chaos_kern::{EditorFileAccess, EditorFileFuture};

pub(crate) struct Editor {
    client: ClientHandle,
    write: bool,
}

impl Editor {
    pub fn negotiated(context: &SessionContext) -> acp_host::protocol::Result<Option<Self>> {
        let fs = context.client().capabilities()?.fs;
        Ok(fs.read_text_file.then(|| Self {
            client: context.client().clone(),
            write: fs.write_text_file,
        }))
    }
}

impl EditorFileAccess for Editor {
    fn can_write(&self) -> bool {
        self.write
    }

    fn read<'a>(&'a self, path: &'a Path) -> EditorFileFuture<'a, String> {
        Box::pin(async move {
            self.client
                .read_text_file(path)
                .await
                .map_err(io::Error::other)
        })
    }

    fn write<'a>(&'a self, path: &'a Path, content: String) -> EditorFileFuture<'a, ()> {
        Box::pin(async move {
            self.client
                .write_text_file(path, content)
                .await
                .map_err(io::Error::other)
        })
    }
}
