use crc32fast::Hasher as Crc32;
use digest::{generic_array::GenericArray, Digest};
use md5::Md5;
use positioned_io::{Cursor, ReadAt, Size};
use reqwest::blocking::Body;
use std::{
    fmt::Debug,
    fs::File,
    io::{copy, Cursor as IOCursor, Read, Result as IOResult},
    sync::{Arc, RwLock},
};

trait ThreadSafeReadDebug: Read + Sync + Send + Debug {}
impl<T: Read + Sync + Send + Debug> ThreadSafeReadDebug for T {}

#[derive(Debug, Clone)]
enum PartibleReader {
    File(Arc<RwLock<File>>),
    Data(Arc<Vec<u8>>),
}

impl PartibleReader {
    #[inline]
    fn len(&self) -> IOResult<u64> {
        match self {
            Self::File(file) => Ok(file.read().unwrap().metadata()?.len()),
            Self::Data(data) => Ok(data.len() as u64),
        }
    }
}

impl ReadAt for PartibleReader {
    #[inline]
    fn read_at(&self, pos: u64, buf: &mut [u8]) -> IOResult<usize> {
        match self {
            Self::File(file) => file.read().unwrap().read_at(pos, buf),
            Self::Data(data) => data.read_at(pos, buf),
        }
    }
}

impl From<Arc<RwLock<File>>> for PartibleReader {
    #[inline]
    fn from(file: Arc<RwLock<File>>) -> Self {
        Self::File(file)
    }
}

impl From<Arc<Vec<u8>>> for PartibleReader {
    #[inline]
    fn from(data: Arc<Vec<u8>>) -> Self {
        Self::Data(data)
    }
}

#[derive(Debug, Clone)]
pub(super) struct PartReader {
    inner: PartReaderInner,
}

#[derive(Debug, Clone)]
enum PartReaderInner {
    ReadAt {
        source: PartibleReader,
        start_from: u64,
        len: u64,
    },
    Data(Arc<Vec<u8>>),
}

impl PartReader {
    #[inline]
    pub(super) fn file(file: Arc<RwLock<File>>, start_from: u64, len: u64) -> Self {
        Self::read_at_based(PartibleReader::File(file), start_from, len)
    }

    #[inline]
    pub(super) fn partible_data(data: Arc<Vec<u8>>, start_from: u64, len: u64) -> Self {
        Self::read_at_based(PartibleReader::Data(data), start_from, len)
    }

    #[inline]
    fn read_at_based(source: PartibleReader, start_from: u64, len: u64) -> Self {
        Self {
            inner: PartReaderInner::ReadAt {
                source,
                start_from,
                len,
            },
        }
    }

    #[inline]
    pub(super) fn data(data: Arc<Vec<u8>>) -> Self {
        Self {
            inner: PartReaderInner::Data(data),
        }
    }

    #[inline]
    pub(super) fn body(&self, size: u64) -> Body {
        Body::sized(self.reader(), size)
    }

    #[inline]
    pub(super) fn md5(&self) -> IOResult<(u64, GenericArray<u8, <Md5 as Digest>::OutputSize>)> {
        let mut hasher = Md5::new();
        let size = copy(&mut self.reader(), &mut hasher)?;
        Ok((size, hasher.finalize()))
    }

    #[inline]
    fn reader(&self) -> Box<dyn Read + Send + 'static> {
        return match &self.inner {
            PartReaderInner::Data(data) => {
                Box::new(Cursor::new(DataReadAtAdapter(data.to_owned())))
            }
            PartReaderInner::ReadAt {
                source,
                start_from,
                len,
            } => Box::new(Cursor::new_pos(source.to_owned(), *start_from).take(*len)),
        };

        #[derive(Debug, Clone)]
        struct DataReadAtAdapter(Arc<Vec<u8>>);

        impl ReadAt for DataReadAtAdapter {
            #[inline]
            fn read_at(&self, pos: u64, buf: &mut [u8]) -> IOResult<usize> {
                self.0.read_at(pos, buf)
            }
        }
    }
}

#[derive(Debug, Clone)]
struct FileReadAtAdapter(Arc<File>);

impl ReadAt for FileReadAtAdapter {
    #[inline]
    fn read_at(&self, pos: u64, buf: &mut [u8]) -> IOResult<usize> {
        self.0.read_at(pos, buf)
    }
}

#[derive(Debug, Clone)]
struct BytesAsRefAdapter(Arc<Vec<u8>>);

impl AsRef<[u8]> for BytesAsRefAdapter {
    #[inline]
    fn as_ref(&self) -> &[u8] {
        &self.0
    }
}

#[derive(Debug, Clone)]
pub(super) struct FormUploadSource {
    inner: FormUploadSourceInner,
}

#[derive(Debug, Clone)]
enum FormUploadSourceInner {
    File(Arc<File>),
    Data(Arc<Vec<u8>>),
}

impl FormUploadSource {
    pub(super) fn crc32(&self) -> IOResult<(u64, u32)> {
        let mut hasher = Crc32::new();
        let mut have_read: u64 = 0;
        let mut reader = self.reader();
        let mut buf = [0u8; 1 << 10];
        loop {
            match reader.read(&mut buf)? {
                0 => {
                    break;
                }
                chunk_size => {
                    hasher.update(&buf[..chunk_size]);
                    have_read = have_read.saturating_add(chunk_size as u64);
                }
            }
        }
        Ok((have_read, hasher.finalize()))
    }

    #[inline]
    pub(super) fn reader(&self) -> impl Read + Send + 'static {
        match &self.inner {
            FormUploadSourceInner::File(file) => {
                FormUploadSourceReader::File(Cursor::new(FileReadAtAdapter(file.to_owned())))
            }
            FormUploadSourceInner::Data(data) => {
                FormUploadSourceReader::Data(IOCursor::new(BytesAsRefAdapter(data.to_owned())))
            }
        }
    }
}

#[derive(Debug)]
enum FormUploadSourceReader {
    File(Cursor<FileReadAtAdapter>),
    Data(IOCursor<BytesAsRefAdapter>),
}

impl Read for FormUploadSourceReader {
    #[inline]
    fn read(&mut self, buf: &mut [u8]) -> IOResult<usize> {
        match self {
            Self::File(file) => file.read(buf),
            Self::Data(data) => data.read(buf),
        }
    }
}

impl From<Arc<File>> for FormUploadSource {
    #[inline]
    fn from(file: Arc<File>) -> Self {
        Self {
            inner: FormUploadSourceInner::File(file),
        }
    }
}

impl From<Arc<Vec<u8>>> for FormUploadSource {
    #[inline]
    fn from(data: Arc<Vec<u8>>) -> Self {
        Self {
            inner: FormUploadSourceInner::Data(data),
        }
    }
}
#[derive(Debug)]
pub(super) struct UploadSource {
    inner: UploadSourceInner,
}

impl From<Arc<RwLock<File>>> for UploadSource {
    #[inline]
    fn from(file: Arc<RwLock<File>>) -> Self {
        Self {
            inner: UploadSourceInner::File(file),
        }
    }
}

impl From<Arc<Vec<u8>>> for UploadSource {
    #[inline]
    fn from(data: Arc<Vec<u8>>) -> Self {
        Self {
            inner: UploadSourceInner::Data(data),
        }
    }
}

#[derive(Debug)]
enum UploadSourceInner {
    File(Arc<RwLock<File>>),
    Reader(Box<dyn ThreadSafeReadDebug>),
    Data(Arc<Vec<u8>>),
}

impl UploadSource {
    #[inline]
    pub(super) fn from_reader(reader: impl Read + Sync + Send + Debug + 'static) -> Self {
        Self {
            inner: UploadSourceInner::Reader(Box::new(reader)),
        }
    }

    #[inline]
    pub(super) fn part(self, part_size: u64) -> IOResult<UploadSourcePartitioner> {
        return match self.inner {
            UploadSourceInner::File(file) if file.read().unwrap().size()?.is_some() => {
                Ok(UploadSourcePartitioner {
                    inner: UploadSourcePartitionerInner::Partible {
                        source: file.into(),
                        offset: 0,
                    },
                    part_size,
                })
            }
            UploadSourceInner::Data(data) => Ok(UploadSourcePartitioner {
                inner: UploadSourcePartitionerInner::Partible {
                    source: data.into(),
                    offset: 0,
                },
                part_size,
            }),
            UploadSourceInner::File(source) => Ok(UploadSourcePartitioner {
                inner: UploadSourcePartitionerInner::Impartible {
                    source: Box::new(ArcLockedFileAdapter(source)),
                },
                part_size,
            }),
            UploadSourceInner::Reader(source) => Ok(UploadSourcePartitioner {
                inner: UploadSourcePartitionerInner::Impartible { source },
                part_size,
            }),
        };

        #[derive(Debug)]
        struct ArcLockedFileAdapter(Arc<RwLock<File>>);

        impl<'a> Read for ArcLockedFileAdapter {
            #[inline]
            fn read(&mut self, buf: &mut [u8]) -> IOResult<usize> {
                self.0.write().unwrap().read(buf)
            }
        }
    }
}

#[derive(Debug)]
pub(super) struct UploadSourcePartitioner {
    inner: UploadSourcePartitionerInner,
    part_size: u64,
}

#[derive(Debug)]
enum UploadSourcePartitionerInner {
    Partible {
        source: PartibleReader,
        offset: u64,
    },
    Impartible {
        source: Box<dyn ThreadSafeReadDebug>,
    },
}

impl UploadSourcePartitioner {
    pub(super) fn next_part_reader(&mut self) -> IOResult<Option<PartReader>> {
        match &mut self.inner {
            UploadSourcePartitionerInner::Partible { source, offset } => {
                let total_size = source.len()?;
                if total_size > *offset {
                    let start_from = *offset;
                    *offset += self.part_size;
                    Ok(Some(PartReader::read_at_based(
                        source.to_owned(),
                        start_from,
                        self.part_size,
                    )))
                } else {
                    Ok(None)
                }
            }
            UploadSourcePartitionerInner::Impartible { source } => {
                let mut part_buf = Vec::new();
                source.take(self.part_size).read_to_end(&mut part_buf)?;
                if part_buf.is_empty() {
                    Ok(None)
                } else {
                    Ok(Some(PartReader::data(Arc::new(part_buf))))
                }
            }
        }
    }
}
