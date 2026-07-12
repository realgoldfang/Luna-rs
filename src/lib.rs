pub mod des;
pub mod zip_writer;

use std::fs;
use std::io::{self, Read, Write, Seek};
use std::path::Path;

fn gnu_basename(path: &str) -> &str {
    path.rfind('/').map(|i| &path[i + 1..]).unwrap_or(path)
}

fn utf8_to_unicode(in_buf: &[u8], offset: usize, end: usize) -> (u32, usize) {
    if offset >= end {
        return (0, offset);
    }

    let b = in_buf[offset];

    if b & 0x80 == 0 {
        return (b as u32, offset + 1);
    }

    if b & 0xE0 == 0xC0 {
        let mut c = (b & 0x1F) as u32;
        if offset + 1 < end {
            c |= (in_buf[offset + 1] & 0x3F) as u32;
        }
        return (c, std::cmp::min(end, offset + 2));
    }

    if b & 0xF0 == 0xE0 {
        let mut c = (b & 0x0F) as u32;
        if offset + 1 < end {
            c |= ((in_buf[offset + 1] & 0x3F) as u32) << 6;
        }
        if offset + 2 < end {
            c |= (in_buf[offset + 2] & 0x3F) as u32;
        }
        return (c, std::cmp::min(end, offset + 3));
    }

    if b & 0xF8 == 0xF0 {
        let mut c = (b & 0x07) as u32;
        if offset + 1 < end {
            c |= ((in_buf[offset + 1] & 0x3F) as u32) << 12;
        }
        if offset + 2 < end {
            c |= ((in_buf[offset + 2] & 0x3F) as u32) << 6;
        }
        if offset + 3 < end {
            c |= (in_buf[offset + 3] & 0x3F) as u32;
        }
        return (c, std::cmp::min(end, offset + 4));
    }

    (0, offset + 1)
}

fn escape_unicode(
    in_buf: &[u8],
    header_size: usize,
    footer_size: usize,
    in_size: usize,
) -> Option<Vec<u8>> {
    let mut out_buf = vec![0u8; header_size + in_size * 4 + footer_size];
    out_buf[..header_size].copy_from_slice(&in_buf[..header_size]);

    let mut p = header_size;

    if p + 3 <= in_buf.len() && &in_buf[p..p + 3] == b"\xEF\xBB\xBF" {
        p += 3;
    }

    let mut op = header_size;
    let end = header_size + in_size;

    while p < end {
        let (uc, new_p) = utf8_to_unicode(in_buf, p, end);
        p = new_p;

        if uc < 0x80 {
            if op >= out_buf.len() {
                return None;
            }
            out_buf[op] = uc as u8;
            op += 1;
        } else if uc < 0x800 {
            if op + 1 >= out_buf.len() {
                return None;
            }
            out_buf[op] = (uc >> 8) as u8;
            out_buf[op + 1] = uc as u8;
            op += 2;
        } else if uc < 0x10000 {
            if op + 2 >= out_buf.len() {
                return None;
            }
            out_buf[op] = 0x80;
            out_buf[op + 1] = (uc >> 8) as u8;
            out_buf[op + 2] = uc as u8;
            op += 3;
        } else {
            if op + 3 >= out_buf.len() {
                return None;
            }
            out_buf[op] = 0x08;
            out_buf[op + 1] = (uc >> 16) as u8;
            out_buf[op + 2] = (uc >> 8) as u8;
            out_buf[op + 3] = uc as u8;
            op += 4;
        }
    }

    out_buf.truncate(op + footer_size);
    Some(out_buf)
}

fn fix_cdata_end_seq(
    mut in_buf: Vec<u8>,
    header_size: usize,
    mut in_size: usize,
) -> Option<Vec<u8>> {
    let cdata_restart = b"]]><![CDATA[";

    let mut offset = header_size;
    while offset + 2 < header_size + in_size {
        if &in_buf[offset..offset + 3] == b"]]>" {
            offset += 2;
            let restart_len = cdata_restart.len();
            let new_size = in_buf.len() + restart_len;
            in_buf.resize(new_size, 0);
            let move_len = header_size + in_size - offset;
            for i in (0..move_len).rev() {
                in_buf[offset + restart_len + i] = in_buf[offset + i];
            }
            in_buf[offset..offset + restart_len].copy_from_slice(cdata_restart);
            in_size += restart_len;
            offset += restart_len;
        } else {
            offset += 1;
        }
    }

    in_buf.truncate(header_size + in_size);
    Some(in_buf)
}

fn reformat_xml_doc(
    in_buf: &[u8],
    header_size: usize,
    in_size: usize,
) -> Option<Vec<u8>> {
    let mut out_buf = vec![0u8; header_size + in_size];
    out_buf[..header_size].copy_from_slice(&in_buf[..header_size]);

    let mut xml_start = None;
    let mut i = header_size;

    while i + 4 < in_buf.len() {
        if in_buf[i..].starts_with(b"<prob") || in_buf[i..].starts_with(b"<doc") {
            xml_start = Some(i);
            break;
        }
        i += 1;
    }

    let xml_start = xml_start?;
    let mut size_written: usize = 0;
    let mut read_offset: usize = 0;
    let size_to_read = in_buf.len() - xml_start;

    let mut tagid_stack: Vec<u32> = Vec::with_capacity(100);
    let mut last_tagid: u32 = 0;
    let out_start = header_size;

    let xml_data = &in_buf[xml_start..];

    while read_offset < size_to_read {
        if xml_data[read_offset] == b'<' {
            if read_offset + 1 >= size_to_read {
                return None;
            }

            if xml_data[read_offset + 1] == b'/' {
                if tagid_stack.is_empty() {
                    return None;
                }
                read_offset += 1;
                let index = tagid_stack.pop().unwrap();

                if index < 256 {
                    if out_start + size_written + 2 > out_buf.len() {
                        out_buf.resize(out_start + size_written + 2, 0);
                    }
                    out_buf[out_start + size_written] = 0x0E;
                    out_buf[out_start + size_written + 1] = index as u8;
                    size_written += 2;
                    while read_offset < size_to_read && xml_data[read_offset] != b'>' {
                        read_offset += 1;
                    }
                } else {
                    let tag_first = read_offset - 1;
                    match xml_data[read_offset..].iter().position(|&c| c == b'>') {
                        Some(pos) => {
                            let tag_last = read_offset + pos;
                            let tag_len = tag_last - tag_first + 1;
                            if out_start + size_written + tag_len > out_buf.len() {
                                out_buf.resize(out_start + size_written + tag_len, 0);
                            }
                            out_buf[out_start + size_written..out_start + size_written + tag_len]
                                .copy_from_slice(&xml_data[tag_first..tag_first + tag_len]);
                            size_written += tag_len;
                            read_offset = tag_last + 1;
                            continue;
                        }
                        None => return None,
                    }
                }

                if read_offset > size_to_read - 1 {
                    return None;
                }
            } else if xml_data[read_offset + 1] == b'!' {
                if read_offset + 2 >= size_to_read {
                    return None;
                }
                read_offset += 2;
                if xml_data[read_offset] == b'-' {
                    return None;
                }
                if read_offset + 9 >= size_to_read {
                    return None;
                }
                if !xml_data[read_offset..read_offset + 7].starts_with(b"[CDATA[") {
                    return None;
                }
                let cdata_first = read_offset - 2;
                read_offset += 7;
                let mut cdata_last = None;
                loop {
                    match xml_data[read_offset..].iter().position(|&c| c == b'>') {
                        Some(pos) => {
                            read_offset += pos + 1;
                            if read_offset >= 3
                                && xml_data[read_offset - 3..read_offset] == *b"]]>"
                            {
                                cdata_last = Some(read_offset - 1);
                                break;
                            }
                        }
                        None => break,
                    }
                }
                if let Some(last) = cdata_last {
                    let cdata_len = last - cdata_first + 1;
                    if out_start + size_written + cdata_len > out_buf.len() {
                        out_buf.resize(out_start + size_written + cdata_len, 0);
                    }
                    out_buf[out_start + size_written..out_start + size_written + cdata_len]
                        .copy_from_slice(&xml_data[cdata_first..cdata_first + cdata_len]);
                    size_written += cdata_len;
                    read_offset = last + 1;
                    continue;
                } else {
                    return None;
                }
            } else {
                if tagid_stack.len() >= 100 {
                    return None;
                }
                tagid_stack.push(last_tagid);
                last_tagid += 1;
                if out_start + size_written < out_buf.len() {
                    out_buf[out_start + size_written] = xml_data[read_offset];
                }
                size_written += 1;
            }
        } else {
            if out_start + size_written < out_buf.len() {
                out_buf[out_start + size_written] = xml_data[read_offset];
            }
            size_written += 1;
        }
        read_offset += 1;
    }

    out_buf.truncate(header_size + size_written);
    Some(out_buf)
}

fn has_ext(filepath: &str, ext: &str) -> bool {
    filepath.len() > ext.len()
        && filepath[filepath.len() - ext.len()..].to_lowercase() == ext.to_lowercase()
}

fn read_file_and_xml_compress(
    inf_path: &str,
    filename_out: &mut String,
) -> Option<Vec<u8>> {
    static LUA_HEADER: &[u8] = b"\x54\x49\x58\x43\x30\x31\x30\x30\x2D\x31\x2E\x30\x3F\x3E\x3C\x70\x72\
        \x6F\x62\x20\x78\x6D\x6C\x6E\x73\x3D\x22\x75\x72\x6E\x3A\x54\x49\x2E\
        \x50\xA8\x5F\x5B\x1F\x0A\x22\x20\x76\x65\x72\x3D\x22\x31\x2E\x30\x22\
        \x20\x70\x62\x6E\x61\x6D\x65\x3D\x22\x22\x3E\x3C\x73\x79\x6D\x3E\x0E\
        \x01\x3C\x63\x61\x72\x64\x20\x63\x6C\x61\x79\x3D\x22\x30\x22\x20\x68\
        \x31\x3D\x22\xF1\x00\x00\xFF\x22\x20\x68\x32\x3D\x22\xF1\x00\x00\xFF\
        \x22\x20\x77\x31\x3D\x22\xF1\x00\x00\xFF\x22\x20\x77\x32\x3D\x22\xF1\
        \x00\x00\xFF\x22\x3E\x3C\x69\x73\x44\x75\x6D\x6D\x79\x43\x61\x72\x64\
        \x3E\x30\x0E\x03\x3C\x66\x6C\x61\x67\x3E\x30\x0E\x04\x3C\x77\x64\x67\
        \x74\x20\x78\x6D\x6C\x6E\x73\x3A\x73\x63\x3D\x22\x75\x72\x6E\x3A\x54\
        \x49\x2E\x53\xAC\x84\xF2\x2A\x41\x70\x70\x22\x20\x74\x79\x70\x65\x3D\
        \x22\x54\x49\x2E\x53\xAC\x84\xF2\x2A\x41\x70\x70\x22\x20\x76\x65\x72\
        \x3D\x22\x31\x2E\x30\x22\x3E\x3C\x73\x63\x3A\x6D\x46\x6C\x61\x67\x73\
        \x3E\x30\x0E\x06\x3C\x73\x63\x3A\x76\x61\x6C\x75\x65\x3E\x2D\x31\x0E\
        \x07\x3C\x73\x63\x3A\x73\x63\x72\x69\x70\x74\x20\x76\x65\x72\x73\x69\
        \x6F\x6E\x3D\x22\x35\x31\x32\x22\x20\x69\x64\x3D\x22\x30\x22\x3E\
        <![CDATA[";

    static LUA_FOOTER: &[u8] = b"]]>\x0E\x08\x0E\x05\x0E\x02\x0E\x00";

    static XML_HEADER: &[u8] = b"\x54\x49\x58\x43\x30\x31\x30\x30\x2D\x31\x2E\x30\x3F\x3E";

    let infile_is_xml = has_ext(inf_path, ".xml");
    let infile_is_lua = has_ext(inf_path, ".lua") || inf_path == "-";

    if inf_path == "-" || infile_is_lua {
        *filename_out = "Problem1.xml".to_string();
    } else {
        *filename_out = gnu_basename(inf_path).to_string();
    }

    let (header, header_size, footer, footer_size) = if infile_is_xml {
        (XML_HEADER as &[u8], XML_HEADER.len(), b"" as &[u8], 0)
    } else if infile_is_lua {
        (LUA_HEADER as &[u8], LUA_HEADER.len(), LUA_FOOTER as &[u8], LUA_FOOTER.len())
    } else {
        (b"" as &[u8], 0, b"" as &[u8], 0)
    };

    let mut in_buf: Vec<u8> = if inf_path == "-" {
        let mut buf = Vec::new();
        io::stdin().read_to_end(&mut buf).ok()?;
        buf
    } else {
        fs::read(inf_path).ok()?
    };

    let in_size = in_buf.len();

    let mut data = Vec::new();
    data.extend_from_slice(header);
    data.append(&mut in_buf);
    data.extend_from_slice(footer);

    if !infile_is_xml && !infile_is_lua {
        return Some(data);
    }

    if infile_is_xml {
        let escaped = escape_unicode(&data, header_size, footer_size, in_size)?;
        reformat_xml_doc(&escaped, header_size, in_size)
    } else {
        data.truncate(header_size + in_size);
        let mut fixed = fix_cdata_end_seq(data, header_size, in_size)?;
        fixed.extend_from_slice(footer);
        Some(fixed)
    }
}

fn doccrypt(inout: &mut [u8]) {
    let cbc1_key: [u8; 8] = [0x16, 0xA7, 0xA7, 0x32, 0x68, 0xA7, 0xBA, 0x73];
    let cbc2_key: [u8; 8] = [0xD9, 0xA8, 0x86, 0xA4, 0x34, 0x45, 0x94, 0x10];
    let cbc3_key: [u8; 8] = [0x3D, 0x80, 0x8C, 0xB5, 0xDF, 0xB3, 0x80, 0x6B];

    let mut ks1 = des::DesKeySchedule { subkeys: [[0; 2]; 16] };
    let mut ks2 = des::DesKeySchedule { subkeys: [[0; 2]; 16] };
    let mut ks3 = des::DesKeySchedule { subkeys: [[0; 2]; 16] };

    des::des_set_key(&cbc1_key, &mut ks1);
    des::des_set_key(&cbc2_key, &mut ks2);
    des::des_set_key(&cbc3_key, &mut ks3);

    let ivec_base: u32 = 0x6fe21307;
    let mut ivec_incr: u32 = 0;

    let mut offset = 0;
    while offset < inout.len() {
        let current_ivec = ivec_base.wrapping_add(ivec_incr);
        ivec_incr += 1;
        if ivec_incr >= 1024 {
            ivec_incr = 0;
        }

        let mut ivec = [0u8; 8];
        ivec[4] = (current_ivec & 0xff) as u8;
        ivec[5] = ((current_ivec >> 8) & 0xff) as u8;
        ivec[6] = ((current_ivec >> 16) & 0xff) as u8;
        ivec[7] = ((current_ivec >> 24) & 0xff) as u8;

        let mut cbc_data = [0u8; 8];
        des::des_ecb3_encrypt(&ivec, &mut cbc_data, &ks1, &ks2, &ks3, true);

        let block_size = std::cmp::min(8, inout.len() - offset);
        for i in 0..block_size {
            inout[offset + i] ^= cbc_data[i];
        }
        offset += 8;
    }
}

fn deflate_compressed_xml(xmlc_buf: &[u8]) -> Vec<u8> {
    unsafe {
        let mut stream = std::mem::MaybeUninit::<libz_sys::z_stream>::uninit();
        std::ptr::write_bytes(stream.as_mut_ptr(), 0, 1);
        let mut stream = stream.assume_init();

        let ret = libz_sys::deflateInit2_(
            &mut stream,
            libz_sys::Z_DEFAULT_COMPRESSION,
            libz_sys::Z_DEFLATED,
            -15, // raw deflate, no zlib header
            8,
            libz_sys::Z_DEFAULT_STRATEGY,
            b"1.3.1\0".as_ptr() as *const i8,
            std::mem::size_of::<libz_sys::z_stream>() as i32,
        );
        if ret != libz_sys::Z_OK {
            panic!("deflateInit2 failed: {}", ret);
        }

        let max_out = xmlc_buf.len() + (xmlc_buf.len() / 1000) + 64;
        let mut out_buf = vec![0u8; max_out];

        stream.next_in = xmlc_buf.as_ptr() as *mut u8;
        stream.avail_in = xmlc_buf.len() as u32;
        stream.next_out = out_buf.as_mut_ptr();
        stream.avail_out = max_out as u32;

        let ret = libz_sys::deflate(&mut stream, libz_sys::Z_FINISH);
        if ret != libz_sys::Z_STREAM_END {
            libz_sys::deflateEnd(&mut stream);
            panic!("deflate failed: {}", ret);
        }

        let total_out = stream.total_out as usize;
        libz_sys::deflateEnd(&mut stream);

        out_buf.truncate(total_out);
        out_buf
    }
}

static DEFAULT_PROCESSED_DOCUMENT_XML: &[u8] = &[
    0x0F, 0xCE, 0xD8, 0xD2, 0x81, 0x06, 0x86, 0x5B, 0x4A, 0x4A, 0xC5, 0xCE, 0xA9, 0x16, 0xF2, 0xD5,
    0x1D, 0xA8, 0x2F, 0x6E, 0x00, 0x22, 0xF2, 0xF0, 0xC1, 0xA6, 0x06, 0x77, 0x4D, 0x7E, 0xA6, 0xC0,
    0x3A, 0xF0, 0x5C, 0x74, 0xBA, 0xAA, 0x44, 0x60, 0xCD, 0x58, 0xE6, 0x70, 0xD7, 0x40, 0xF6, 0x9C,
    0x17, 0xDC, 0xF0, 0x94, 0x77, 0xBF, 0xCA, 0xDE, 0xF7, 0x02, 0x09, 0xC9, 0x62, 0xB1, 0x5D, 0xEF,
    0x22, 0xFA, 0x51, 0x37, 0xA0, 0x81, 0x91, 0x48, 0xE1, 0x83, 0x4D, 0xAD, 0x08, 0x31, 0x2D, 0xD0,
    0xD3, 0xE3, 0x2D, 0x60, 0xAB, 0x13, 0xC2, 0x98, 0x2B, 0xED, 0x39, 0x5B, 0x09, 0x24, 0x39, 0x92,
    0x2F, 0x0C, 0x7A, 0x4C, 0x95, 0x74, 0x91, 0x3B, 0x0C, 0xF4, 0x60, 0xCC, 0x73, 0x27, 0xCB, 0x07,
    0x7E, 0x7F, 0xA9, 0x17, 0x87, 0xE2, 0xAC, 0xA2, 0x3B, 0xCC, 0xA0, 0xC4, 0xE3, 0x8E, 0x89, 0xF0,
    0xC0, 0x51, 0x9F, 0xC2, 0xBE, 0xCE, 0x28, 0x45, 0xC3, 0xD4, 0x11, 0x90, 0xA6, 0xEC, 0x53, 0xA0,
    0xFB, 0x5B, 0x46, 0x6B, 0x41, 0xAD, 0xE9, 0x53, 0xBB, 0x97, 0xDB, 0xB1, 0xD2, 0x68, 0xE2, 0xF6,
    0x36, 0x0F, 0x26, 0x36, 0x75, 0x9B, 0xE9, 0x1F, 0x48, 0xAD, 0xE9, 0x29, 0x67, 0x00, 0x58, 0x19,
    0xC3, 0xC0, 0x12, 0x76, 0xA0, 0x4A, 0x73, 0xF3, 0xB1, 0xD3, 0x09, 0x18, 0xD6, 0x06, 0xDD, 0x97,
    0x24, 0x53, 0x3E, 0x22, 0xA4, 0xFB, 0x82, 0x50, 0x7B, 0x7C, 0x12, 0x88, 0x4E, 0x7D, 0x41, 0x80,
    0xFE, 0x72, 0x92, 0x29, 0x87, 0xE8, 0x5C, 0x56, 0x72, 0xFF, 0x29, 0x16, 0x8C, 0x42, 0x5B, 0x8B,
    0x9B, 0xA7, 0xD2, 0x08, 0x6D, 0xD3, 0x98, 0xFF, 0x91, 0xA9, 0x9E, 0xF3, 0x93, 0xA8, 0x2E, 0x1C,
    0xB2, 0xA9, 0x6B, 0x6A, 0xDF, 0xF6, 0xCE, 0x2D, 0x15, 0x17, 0xCE, 0x6E, 0xC0, 0x4F, 0x9A, 0x9C,
    0x0E, 0xDF, 0x19, 0x8D, 0x2D, 0xFA, 0x69, 0x9F, 0x11, 0xD2, 0x20, 0x12, 0xE0, 0x79, 0x14, 0x04,
    0x4E, 0x62, 0x8F, 0x0A, 0x2A, 0x18, 0x72, 0x5A, 0x8B, 0x80, 0xB3, 0x3C, 0x9B, 0xD5, 0x67, 0x59,
    0x4B, 0x51, 0x4D, 0xE0, 0xC3, 0x38, 0x28, 0xC3, 0xDC, 0xCD, 0x39, 0x22, 0x12, 0x8C, 0x40, 0x55,
];

static TIEN_CRYPTED_HEADER: &[u8] = &[
    0x0F, 0xCE, 0xD8, 0xD2, 0x81, 0x06, 0x86, 0x5B, 0x99, 0xDD, 0xA2, 0x3D, 0xD9, 0xE9, 0x4B, 0xD4,
    0x31, 0xBB, 0x50, 0xB6, 0x4D, 0xB3, 0x29, 0x24, 0x70, 0x60, 0x49, 0x38, 0x1C, 0x30, 0xF8, 0x99,
    0x00, 0x4B, 0x92, 0x64, 0xE4, 0x58, 0xE6, 0xBC,
];

fn add_compressed_xml_to_tns<W: Write + Seek>(
    writer: &mut zip_writer::TiZipWriter<W>,
    filename: &str,
    xmlc_buf: &[u8],
    tiversion: u32,
) -> io::Result<()> {
    let mut def_buf = deflate_compressed_xml(xmlc_buf);

    doccrypt(&mut def_buf);

    let mut header_and_deflated = Vec::new();
    header_and_deflated.extend_from_slice(TIEN_CRYPTED_HEADER);
    header_and_deflated.extend_from_slice(&def_buf);

    writer.add_processed_file(filename, &header_and_deflated, tiversion)
}

fn add_default_document_to_tns<W: Write + Seek>(
    writer: &mut zip_writer::TiZipWriter<W>,
    tiversion: u32,
) -> io::Result<()> {
    writer.add_processed_file("Document.xml", DEFAULT_PROCESSED_DOCUMENT_XML, tiversion)
}

fn add_python_xml_to_tns<W: Write + Seek>(
    writer: &mut zip_writer::TiZipWriter<W>,
    python_path: &str,
    tiversion: u32,
) -> io::Result<()> {
    let py_header = b"TIXC0100-1.0?><prob xmlns=\"urn:TI.Problem\" ver=\"1.0\" pbname=\"\">\
        <sym>\x0E\x01<card clay=\"0\" h1=\"10000\" h2=\"10000\" w1=\"10000\" \
        w2=\"10000\"><isDummyCard>0\x0E\x03<flag>0\x0E\x04<wdgt xmlns:py=\"urn:\
        TI.PythonEditor\" type=\"TI.PythonEditor\" ver=\"1.0\"><py:data><py:name>";

    let py_footer = b"\x0E\x07<py:dirf>-10000000\x0E\x08\x0E\x06<py:mFlags>1024\x0E\x09\
        <py:value>10\x0E\x0A\x0E\x05\x0E\x02\x0E\x00";

    let filename = gnu_basename(python_path);
    if filename.len() > 240 {
        return Err(io::Error::new(io::ErrorKind::InvalidInput, "filename too long"));
    }

    let mut xmlc_buf = Vec::new();
    xmlc_buf.extend_from_slice(py_header);
    xmlc_buf.extend_from_slice(filename.as_bytes());
    xmlc_buf.extend_from_slice(py_footer);

    let total_size = py_header.len() + filename.len() + py_footer.len();
    add_compressed_xml_to_tns(writer, "Problem1.xml", &xmlc_buf[..total_size], tiversion)
}

fn add_infile_to_tns<W: Write + Seek>(
    writer: &mut zip_writer::TiZipWriter<W>,
    infile_path: &str,
    tiversion: u32,
) -> io::Result<()> {
    let mut filename = String::new();
    let xmlc_buf = read_file_and_xml_compress(infile_path, &mut filename)
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidData, "failed to read and compress file"))?;

    if has_ext(&filename, ".xml") {
        add_compressed_xml_to_tns(writer, &filename, &xmlc_buf, tiversion)
    } else {
        writer.add_processed_file(&filename, &xmlc_buf, tiversion)
    }
}

/// Compile a Lua source file to a .tns file
pub fn compile_lua(input_path: &str, output_path: &str) -> io::Result<()> {
    if !Path::new(input_path).exists() {
        return Err(io::Error::new(io::ErrorKind::NotFound, format!("Input file not found: {}", input_path)));
    }

    let output_file = fs::File::create(output_path)?;
    let mut writer = zip_writer::TiZipWriter::new(io::BufWriter::new(output_file));

    add_default_document_to_tns(&mut writer, 0x0500)?;
    add_infile_to_tns(&mut writer, input_path, 0x0500)?;

    writer.close()
}

/// Compile one or more Python source files to a .tns file
pub fn compile_python(input_paths: &[String], output_path: &str) -> io::Result<()> {
    for path in input_paths {
        if !Path::new(path).exists() {
            return Err(io::Error::new(io::ErrorKind::NotFound, format!("Input file not found: {}", path)));
        }
    }

    let output_file = fs::File::create(output_path)?;
    let mut writer = zip_writer::TiZipWriter::new(io::BufWriter::new(output_file));

    let tiversion = 0x0500;

    add_default_document_to_tns(&mut writer, tiversion)?;

    for (i, path) in input_paths.iter().enumerate() {
        if i == 0 {
            add_python_xml_to_tns(&mut writer, path, tiversion)?;
        }
        add_infile_to_tns(&mut writer, path, tiversion)?;
    }

    writer.close()
}

/// Compile an XML (Notes) document to a .tns file
pub fn compile_notes_xml(xml_content: &str, output_path: &str) -> io::Result<()> {
    let output_file = fs::File::create(output_path)?;
    let mut writer = zip_writer::TiZipWriter::new(io::BufWriter::new(output_file));

    add_default_document_to_tns(&mut writer, 0x0500)?;

    let mut filename = String::new();
    let xmlc_buf = read_file_and_xml_compress_bytes(xml_content.as_bytes(), &mut filename)
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidData, "failed to process XML content"))?;

    add_compressed_xml_to_tns(&mut writer, &filename, &xmlc_buf, 0x0500)?;

    writer.close()
}

/// Compile a Notes text file to a .tns file
pub fn compile_notes(text: &str, output_path: &str) -> io::Result<()> {
    let xml = format!(
        r#"<?xml version="1.0" encoding="UTF-8"?>
<Problem>
  <ProblemID>1</ProblemID>
  <Page>
    <PageID>1</PageID>
    <Notes>
      <Text><![CDATA[{}]]></Text>
    </Notes>
  </Page>
</Problem>"#,
        text
    );
    compile_notes_xml(&xml, output_path)
}

/// Compile source code to a .tns file based on the file extension
pub fn compile(source_type: &str, input_path: &str, output_path: &str) -> io::Result<()> {
    match source_type.to_lowercase().as_str() {
        "lua" => compile_lua(input_path, output_path),
        "py" | "python" => compile_python(&[input_path.to_string()], output_path),
        "xml" | "notes" => {
            let content = fs::read_to_string(input_path)?;
            compile_notes_xml(&content, output_path)
        }
        _ => Err(io::Error::new(io::ErrorKind::InvalidInput, format!("Unknown source type: {}", source_type))),
    }
}

/// Helper to process XML content from bytes (used internally)
fn read_file_and_xml_compress_bytes(
    in_buf: &[u8],
    filename_out: &mut String,
) -> Option<Vec<u8>> {
    *filename_out = "Problem1.xml".to_string();

    static XML_HEADER: &[u8] = b"\x54\x49\x58\x43\x30\x31\x30\x30\x2D\x31\x2E\x30\x3F\x3E";

    let header_size = XML_HEADER.len();
    let footer_size = 0;

    let mut data = Vec::new();
    data.extend_from_slice(XML_HEADER);
    data.extend_from_slice(in_buf);

    let in_size = in_buf.len();

    let escaped = escape_unicode(&data, header_size, footer_size, in_size)?;
    reformat_xml_doc(&escaped, header_size, in_size)
}
