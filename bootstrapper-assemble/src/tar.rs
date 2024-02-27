use bimap::BiBTreeMap;
use std::{
    io::{Cursor, Read},
    path::{Component, PathBuf},
};

use tar::Header;

pub struct TarArchiveReader<'a> {
    data: &'a [u8],
    archive: tar::Archive<Cursor<&'a [u8]>>,
}

impl<'a> From<&'a [u8]> for TarArchiveReader<'a> {
    fn from(value: &'a [u8]) -> Self {
        Self {
            data: value,
            archive: tar::Archive::new(Cursor::new(value)),
        }
    }
}

impl<'a> TarArchiveReader<'a> {
    pub fn reset(&mut self) {
        self.archive = tar::Archive::new(Cursor::new(self.data));
    }
    pub fn file_contents(&mut self, path: PathBuf) -> Vec<u8> {
        let path = PathBuf::from(
            path.as_os_str()
                .to_str()
                .unwrap()
                .trim_matches('/')
                .to_owned()
                + "/",
        );
        for i in self.archive.entries().unwrap() {
            let mut i = i.unwrap();
            if i.path().unwrap() == path {
                let mut buf = Vec::new();
                i.read_to_end(&mut buf).unwrap();
                return buf;
            }
        }
        panic!("Did not find entry {:?} in archive", path);
    }
}

pub struct ZipArchiveReader<'a> {
    archive: zip::ZipArchive<Cursor<&'a [u8]>>,
}

impl<'a> From<&'a [u8]> for ZipArchiveReader<'a> {
    fn from(value: &'a [u8]) -> Self {
        Self {
            archive: zip::ZipArchive::new(Cursor::new(value)).unwrap(),
        }
    }
}

pub struct TarArchiveWriter<'a> {
    builder: tar::Builder<Cursor<&'a mut Vec<u8>>>,
}

impl<'a> From<&'a mut Vec<u8>> for TarArchiveWriter<'a> {
    fn from(value: &'a mut Vec<u8>) -> Self {
        Self {
            builder: tar::Builder::new(Cursor::new(value)),
        }
    }
}

impl<'a> TarArchiveWriter<'a> {
    pub fn create_empty_dir(&mut self, path: PathBuf) -> Result<(), std::io::Error> {
        let path = PathBuf::from(
            path.as_os_str()
                .to_str()
                .unwrap()
                .trim_matches('/')
                .to_owned()
                + "/",
        );
        let mut header = Header::new_gnu();
        header.set_path(path).unwrap();
        header.set_size(0);
        header.set_mtime(1153704088);
        header.set_mode(0o755);
        header.set_uid(0);
        header.set_gid(0);
        header.set_cksum();
        self.builder.append(&header, &mut std::io::empty())?;
        Ok(())
    }

    pub fn create_file(
        &mut self,
        path: PathBuf,
        data: &[u8],
        mode: Option<u32>,
    ) -> Result<(), std::io::Error> {
        let path = PathBuf::from(path.as_os_str().to_str().unwrap().trim_matches('/'));
        let mut header = Header::new_gnu();
        header.set_size(data.len().try_into().unwrap());
        header.set_mtime(1153704088);
        header.set_mode(if let Some(mode) = mode { mode } else { 0o644 });
        header.set_uid(0);
        header.set_gid(0);
        header.set_cksum();
        self.builder
            .append_data(&mut header, path, std::io::Cursor::new(data))?;
        Ok(())
    }

    pub fn finish(&mut self) -> Result<(), std::io::Error> {
        self.builder.finish()
    }

    pub fn copy_from(
        &mut self,
        reader: &mut ArchiveReader,
        in_prefix: PathBuf,
        out_prefix: PathBuf,
    ) {
        match reader {
            ArchiveReader::TAR(tar) => self.copy_from_tar(tar, in_prefix, out_prefix),
            ArchiveReader::ZIP(zip) => self.copy_from_zip(zip, in_prefix, out_prefix),
        }
    }

    pub fn add_entry(&mut self, e: tar::Entry<Cursor<&[u8]>>, outpath: PathBuf) {
        if e.header().entry_type().is_file() {
            let mut eh = e.header().clone();
            eh.set_path(outpath).unwrap();
            eh.set_cksum();
            self.builder.append(&eh, e).unwrap();
        } else if e.header().entry_type().is_dir() {
            let mut eh = e.header().clone();
            eh.set_path(outpath).unwrap();
            eh.set_cksum();
            self.builder.append(&eh, std::io::empty()).unwrap();
        } else if e.header().entry_type().is_symlink() {
            let mut eh = e.header().clone();
            eh.set_path(outpath.clone()).unwrap();
            eh.set_cksum();
            self.builder
                .append_link(&mut eh, outpath, e.link_name().unwrap().unwrap())
                .unwrap();
        } else if e.header().entry_type().is_hard_link() {
            let mut eh = e.header().clone();
            eh.set_path(outpath).unwrap();
            eh.set_cksum();
            self.builder.append(&eh, std::io::empty()).unwrap();
        } else {
            println!("{:?}", e.header().entry_type());
            todo!();
        }
    }

    pub fn copy_from_tar(
        &mut self,
        tar: &mut TarArchiveReader,
        in_prefix: PathBuf,
        out_prefix: PathBuf,
    ) {
        let in_prefix = PathBuf::from(
            in_prefix
                .as_os_str()
                .to_str()
                .unwrap()
                .trim_start_matches('/'),
        );
        let out_prefix = PathBuf::from(
            out_prefix
                .as_os_str()
                .to_str()
                .unwrap()
                .trim_start_matches('/'),
        );
        for e in tar.archive.entries().unwrap() {
            let e = e.unwrap();
            if let Ok(outpath) = e.header().path().unwrap().strip_prefix(in_prefix.clone()) {
                let outpath = if outpath.as_os_str().is_empty() {
                    out_prefix.clone()
                } else {
                    out_prefix.join(outpath)
                };
                if !outpath.as_os_str().is_empty() {
                    self.add_entry(e, outpath);
                }
            }
        }
        tar.reset();
    }

    pub fn copy_from_zip(
        &mut self,
        zip: &mut ZipArchiveReader,
        in_prefix: PathBuf,
        out_prefix: PathBuf,
    ) {
        let in_prefix = PathBuf::from(
            in_prefix
                .as_os_str()
                .to_str()
                .unwrap()
                .trim_start_matches('/'),
        );
        let out_prefix = PathBuf::from(
            out_prefix
                .as_os_str()
                .to_str()
                .unwrap()
                .trim_start_matches('/'),
        );
        let file_names: Vec<_> = zip.archive.file_names().map(|x| x.to_owned()).collect();
        let mut try_prefix_dir = PathBuf::from(&file_names[0])
            .components()
            .filter_map(|x| {
                if let Component::Normal(v) = x {
                    Some(v)
                } else {
                    None
                }
            })
            .next()
            .map(|x| x.to_owned());
        for i in &file_names {
            if let Some(v) = &try_prefix_dir {
                if PathBuf::from(&i)
                    .components()
                    .filter_map(|x| {
                        if let Component::Normal(v) = x {
                            Some(v)
                        } else {
                            None
                        }
                    })
                    .next()
                    .map(|x| x.to_owned())
                    != Some(v.to_owned())
                {
                    try_prefix_dir = None;
                }
            } else {
                break;
            }
        }
        for e in file_names {
            let mut e = zip.archive.by_name(&e).unwrap();
            let file_name = if let Some(v) = &try_prefix_dir {
                e.enclosed_name()
                    .unwrap()
                    .strip_prefix(v)
                    .unwrap()
                    .strip_prefix(in_prefix.clone())
            } else {
                e.enclosed_name().unwrap().strip_prefix(in_prefix.clone())
            };
            if let Ok(outpath) = file_name {
                let outpath = out_prefix.join(outpath);
                if !outpath.as_os_str().is_empty() {
                    if e.is_dir() {
                        //self.create_empty_dir(outpath.clone()).unwrap();
                    } else if e.is_file() {
                        let outpath = PathBuf::from(
                            outpath.as_os_str().to_str().unwrap().trim_end_matches('/'),
                        );
                        let mut buf = Vec::new();
                        e.read_to_end(&mut buf).unwrap();
                        self.create_file(outpath.clone(), &buf, e.unix_mode())
                            .unwrap();
                    } else {
                        todo!();
                    }
                }
            }
        }
    }
}

pub enum ArchiveReader<'a> {
    TAR(TarArchiveReader<'a>),
    ZIP(ZipArchiveReader<'a>),
}

pub fn flatten_tar(v: Vec<u8>) -> Vec<u8> {
    let mut path_index = BiBTreeMap::new();
    let mut tar = tar::Archive::new(Cursor::new(v.as_slice()));
    for (j, entry) in tar.entries_with_seek().unwrap().enumerate() {
        let entry = entry.unwrap();
        let path = entry.path().unwrap().into_owned();
        path_index.insert(j, path.clone());
    }
    let mut tar = tar::Archive::new(Cursor::new(v.as_slice()));
    let mut outbuf = Vec::new();
    let mut taw = TarArchiveWriter::from(&mut outbuf);
    for (j, entry) in tar.entries_with_seek().unwrap().enumerate() {
        let entry = entry.unwrap();
        let path = entry.path().unwrap().into_owned();
        if path_index.contains_left(&j) {
            taw.add_entry(entry, path);
        }
    }
    std::mem::drop(taw);
    outbuf
}
