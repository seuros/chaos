use super::super::super::{LogQuery, LogRow, StateRuntime};
use super::super::tail::logs_tail_backend;
use crate::{LogTailBatch, LogTailCursor};

logs_tail_backend!(pub impl StateRuntime);
