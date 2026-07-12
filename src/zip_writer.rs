// TI-Nspire ZIP writer - ported from minizip-1.1 with TI modifications
use std::io::{self, Write, Seek, SeekFrom};

const LOCALHEADERMAGIC1: u32 = 0x4D49542A; // MIT*
const LOCALHEADERMAGIC2: u16 = 0x504C; // PL
const STDLOCALHEADERMAGIC: u32 = 0x04034b50;
const CENTRALHEADERMAGIC: u32 = 0x02014b50;
const ENDHEADERMAGIC: u32 = 0x44504954; // DPIT

pub struct TiZipWriter<W: Write + Seek> {
    writer: W,
    entries: Vec<CentralDirEntry>,
    number_entry: u64,
}

struct CentralDirEntry {
    filename: Vec<u8>,
    method: u16,
    dos_date: u32,
    crc32: u32,
    compressed_size: u32,
    uncompressed_size: u32,
    local_header_offset: u64,
    flag: u16,
    internal_attr: u16,
}

impl<W: Write + Seek> TiZipWriter<W> {
    pub fn new(writer: W) -> Self {
        TiZipWriter {
            writer,
            entries: Vec::new(),
            number_entry: 0,
        }
    }

    pub fn add_processed_file(
        &mut self,
        filename: &str,
        data: &[u8],
        tiversion: u32,
    ) -> io::Result<()> {
        let method: u16;
        let level: u32;

        if filename.to_lowercase().ends_with(".xml") {
            method = 0x000D; // TI encrypted (store)
            level = 0;
        } else {
            method = 0x0008; // Z_DEFLATED
            level = 6; // Z_DEFAULT_COMPRESSION
        }

        let flag: u16 = if level == 8 || level == 9 {
            2
        } else if level == 2 {
            4
        } else if level == 1 {
            6
        } else {
            0
        };

        let header_offset = self.writer.seek(SeekFrom::Current(0))?;

        // Write local file header
        if self.number_entry == 0 {
            // First entry: MIT*PL + tiversion
            write_u32(&mut self.writer, LOCALHEADERMAGIC1)?;
            write_u16(&mut self.writer, LOCALHEADERMAGIC2)?;
            let tiversion_str = format!("{:04X}", tiversion);
            self.writer.write_all(tiversion_str.as_bytes())?;
        } else {
            write_u32(&mut self.writer, STDLOCALHEADERMAGIC)?;
        }

        write_u16(&mut self.writer, 20)?; // version needed
        write_u16(&mut self.writer, flag)?;
        write_u16(&mut self.writer, method)?;
        write_u32(&mut self.writer, tmz_date_to_dos_date(0, 0, 0, 0, 0, 0))?; // DOS date
        write_u32(&mut self.writer, 0)?; // CRC32 placeholder
        write_u32(&mut self.writer, 0)?; // compressed size placeholder
        write_u32(&mut self.writer, 0)?; // uncompressed size placeholder
        write_u16(&mut self.writer, filename.len() as u16)?;
        write_u16(&mut self.writer, 0)?; // extra field length

        self.writer.write_all(filename.as_bytes())?;

        let crc_offset = if self.number_entry == 0 {
            header_offset + 10 // 4 (MIT*) + 2 (PL) + 4 (version hex)
        } else {
            header_offset + 4
        } + 2 + 2 + 2 + 4; // version + flag + method + dos_date

        // Write file data
        let data_start = self.writer.seek(SeekFrom::Current(0))?;

        let mut internal_attr: u16 = 0;

        if method == 0x0008 {
            // Deflate (raw, windowBits=-15)
            #[allow(invalid_value)]
            unsafe {
                let mut stream = std::mem::MaybeUninit::<libz_sys::z_stream>::uninit();
                std::ptr::write_bytes(stream.as_mut_ptr(), 0, 1);
                let mut stream = stream.assume_init();
                let ret = libz_sys::deflateInit2_(
                    &mut stream,
                    level as i32,
                    libz_sys::Z_DEFLATED,
                    -15,
                    8,
                    libz_sys::Z_DEFAULT_STRATEGY,
                    b"1.2.13\0".as_ptr() as *const i8,
                    std::mem::size_of::<libz_sys::z_stream>() as i32,
                );
                if ret != libz_sys::Z_OK {
                    return Err(io::Error::new(io::ErrorKind::Other, format!("deflateInit2 failed: {}", ret)));
                }

                stream.next_in = data.as_ptr() as *mut u8;
                stream.avail_in = data.len() as u32;
                let mut out_chunk = [0u8; 16384];
                loop {
                    stream.next_out = out_chunk.as_mut_ptr();
                    stream.avail_out = out_chunk.len() as u32;
                    let ret = libz_sys::deflate(&mut stream, libz_sys::Z_FINISH);
                    let written = out_chunk.len() - stream.avail_out as usize;
                    self.writer.write_all(&out_chunk[..written])?;
                    if ret == libz_sys::Z_STREAM_END {
                        break;
                    }
                    if ret != libz_sys::Z_OK {
                        libz_sys::deflateEnd(&mut stream);
                        return Err(io::Error::new(io::ErrorKind::Other, format!("deflate failed: {}", ret)));
                    }
                }
                // Z_ASCII = 1 = Z_TEXT
                if stream.data_type & 0x0f == 1 {
                    internal_attr = 1;
                }
                libz_sys::deflateEnd(&mut stream);
            }
        } else {
            // Store
            self.writer.write_all(data)?;
        }

        let data_end = self.writer.seek(SeekFrom::Current(0))?;
        let compressed_size = (data_end - data_start) as u32;
        let uncompressed_size = data.len() as u32;
        let crc = crc32fast::hash(data);

        // Seek back and write CRC and sizes
        self.writer.seek(SeekFrom::Start(crc_offset))?;
        write_u32(&mut self.writer, crc)?;
        write_u32(&mut self.writer, compressed_size)?;
        write_u32(&mut self.writer, uncompressed_size)?;

        // Seek back to end
        self.writer.seek(SeekFrom::Start(data_end))?;

        // Add to central directory
        self.entries.push(CentralDirEntry {
            filename: filename.as_bytes().to_vec(),
            method,
            dos_date: tmz_date_to_dos_date(0, 0, 0, 0, 0, 0),
            crc32: crc,
            compressed_size,
            uncompressed_size,
            local_header_offset: header_offset,
            flag,
            internal_attr,
        });

        self.number_entry += 1;
        Ok(())
    }

    pub fn close(mut self) -> io::Result<()> {
        // Close any open file first (not needed in our design)

        let central_dir_offset = self.writer.seek(SeekFrom::Current(0))?;

        // Write central directory entries
        for entry in &self.entries {
            write_u32(&mut self.writer, CENTRALHEADERMAGIC)?;
            write_u16(&mut self.writer, 0)?; // version made by
            write_u16(&mut self.writer, 20)?; // version needed
            write_u16(&mut self.writer, entry.flag)?;
            write_u16(&mut self.writer, entry.method)?;
            write_u32(&mut self.writer, entry.dos_date)?;
            write_u32(&mut self.writer, entry.crc32)?;
            write_u32(&mut self.writer, entry.compressed_size)?;
            write_u32(&mut self.writer, entry.uncompressed_size)?;
            write_u16(&mut self.writer, entry.filename.len() as u16)?;
            write_u16(&mut self.writer, 0)?; // extra field length
            write_u16(&mut self.writer, 0)?; // comment length
            write_u16(&mut self.writer, 0)?; // disk number start
            write_u16(&mut self.writer, entry.internal_attr)?; // internal file attributes
            write_u32(&mut self.writer, 0)?; // external file attributes
            write_u32(&mut self.writer, entry.local_header_offset as u32)?;

            self.writer.write_all(&entry.filename)?;
        }

        let central_dir_size = self.writer.seek(SeekFrom::Current(0))? - central_dir_offset;

        // Write end of central directory
        write_u32(&mut self.writer, ENDHEADERMAGIC)?;
        write_u16(&mut self.writer, 0)?; // number of this disk
        write_u16(&mut self.writer, 0)?; // number of disk with central dir
        write_u16(&mut self.writer, self.entries.len() as u16)?;
        write_u16(&mut self.writer, self.entries.len() as u16)?;
        write_u32(&mut self.writer, central_dir_size as u32)?;
        write_u32(&mut self.writer, central_dir_offset as u32)?;
        write_u16(&mut self.writer, 0)?; // global comment length

        Ok(())
    }
}

fn tmz_date_to_dos_date(tm_year: u32, tm_mon: u32, tm_mday: u32, tm_hour: u32, tm_min: u32, tm_sec: u32) -> u32 {
    let mut year = tm_year;
    if year >= 1980 {
        year -= 1980;
    } else if year >= 80 {
        year -= 80;
    }
    ((tm_mday + 32 * (tm_mon + 1) + 512 * year) << 16) | ((tm_sec / 2) + 32 * tm_min + 2048 * tm_hour)
}

fn write_u16(writer: &mut impl Write, val: u16) -> io::Result<()> {
    writer.write_all(&val.to_le_bytes())
}

fn write_u32(writer: &mut impl Write, val: u32) -> io::Result<()> {
    writer.write_all(&val.to_le_bytes())
}
