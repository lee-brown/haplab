//! QuickTime MOV container muxing and demuxing modules.

pub mod atoms;
pub mod reader;
pub mod writer;

pub use reader::{FrameSample, MovReaderError, QtHapReader};
pub use writer::{MovWriterError, QtHapWriter, VideoConfig};
