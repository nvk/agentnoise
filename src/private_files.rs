//! Creation-time permissions for private job metadata and logs.
//! Existing/shared parent directories are deliberately not chmodded.
use std::fs::{self, File, OpenOptions};
use std::io::{self, Write};
use std::path::Path;

pub(crate) fn create_dir_all(path: &Path) -> io::Result<()> {
    let mut builder = fs::DirBuilder::new();
    builder.recursive(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::DirBuilderExt;
        builder.mode(0o700);
    }
    builder.create(path)
}

pub(crate) fn create_new(path: &Path) -> io::Result<File> {
    let mut options = OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    // Exclusive creation also refuses a pre-existing file/symlink at this path.
    options.open(path)
}

pub(crate) fn write_new(path: &Path, contents: &[u8]) -> io::Result<()> {
    create_new(path)?.write_all(contents)
}
