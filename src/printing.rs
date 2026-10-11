//! Bounded raster PDF for PrintKit, using the original email renderer.
use std::{
    fs::OpenOptions,
    io::{Seek, Write},
    path::Path,
};

pub(crate) fn write_mail_pdf(path: &Path, html: &str) -> Result<(), String> {
    let mut options = OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let mut file = options.open(path).map_err(|e| e.to_string())?;
    let result = (|| -> Result<(), String> {
        file.write_all(b"%PDF-1.4\n%\xE2\xE3\xCF\xD3\n")
            .map_err(|e| e.to_string())?;
        let mut offsets = vec![0u64];
        let mut total_pages = 0;
        crate::renderer::print_pages(html, |page, count, image| {
            if page == 0 {
                total_pages = count;
                object(
                    &mut file,
                    &mut offsets,
                    1,
                    b"<< /Type /Catalog /Pages 2 0 R >>",
                )?;
                let kids = (0..count)
                    .map(|p| format!("{} 0 R", 3 + p * 3))
                    .collect::<Vec<_>>()
                    .join(" ");
                object(
                    &mut file,
                    &mut offsets,
                    2,
                    format!("<< /Type /Pages /Count {count} /Kids [{kids}] >>").as_bytes(),
                )?;
            }
            let pixels = image.to_rgb8().ok_or("Print renderer returned no pixels")?;
            let mut jpeg = Vec::new();
            image::codecs::jpeg::JpegEncoder::new_with_quality(&mut jpeg, 95)
                .encode(
                    pixels.as_bytes(),
                    pixels.width(),
                    pixels.height(),
                    image::ExtendedColorType::Rgb8,
                )
                .map_err(|e| e.to_string())?;
            if file
                .stream_position()
                .map_err(|e| e.to_string())?
                .saturating_add(jpeg.len() as u64)
                > 64 * 1024 * 1024
            {
                return Err("Printed PDF exceeds 64 MiB".into());
            }
            let id = 3 + page * 3;
            object(&mut file,&mut offsets,id,format!("<< /Type /Page /Parent 2 0 R /MediaBox [0 0 595.28 841.89] /Resources << /XObject << /Im0 {} 0 R >> >> /Contents {} 0 R >>",id+2,id+1).as_bytes())?;
            let ratio = pixels.height() as f64 / pixels.width() as f64;
            let width = 595.28_f64.min(841.89 / ratio);
            let height = width * ratio;
            let content = format!(
                "q {width:.4} 0 0 {height:.4} {:.4} {:.4} cm /Im0 Do Q\n",
                (595.28 - width) / 2.0,
                841.89 - height
            );
            stream(&mut file, &mut offsets, id + 1, "", content.as_bytes())?;
            stream(
                &mut file,
                &mut offsets,
                id + 2,
                &format!(
                    "/Type /XObject /Subtype /Image /Width {} /Height {} /ColorSpace /DeviceRGB /BitsPerComponent 8 /Filter /DCTDecode",
                    pixels.width(),
                    pixels.height()
                ),
                &jpeg,
            )?;
            Ok(())
        })?;
        if total_pages == 0 {
            return Err("No printable mail content".into());
        }
        let start = file.stream_position().map_err(|e| e.to_string())?;
        write!(file, "xref\n0 {}\n0000000000 65535 f \n", offsets.len())
            .map_err(|e| e.to_string())?;
        for offset in &offsets[1..] {
            writeln!(file, "{offset:010} 00000 n ").map_err(|e| e.to_string())?;
        }
        write!(
            file,
            "trailer\n<< /Size {} /Root 1 0 R >>\nstartxref\n{start}\n%%EOF\n",
            offsets.len()
        )
        .map_err(|e| e.to_string())?;
        file.sync_all().map_err(|e| e.to_string())
    })();
    drop(file);
    if result.is_err() {
        let _ = std::fs::remove_file(path);
    }
    result
}
fn object(
    file: &mut std::fs::File,
    offsets: &mut Vec<u64>,
    id: u32,
    bytes: &[u8],
) -> Result<(), String> {
    offsets.push(file.stream_position().map_err(|e| e.to_string())?);
    writeln!(file, "{id} 0 obj")
        .and_then(|_| file.write_all(bytes))
        .and_then(|_| file.write_all(b"\nendobj\n"))
        .map_err(|e| e.to_string())
}
fn stream(
    file: &mut std::fs::File,
    offsets: &mut Vec<u64>,
    id: u32,
    attributes: &str,
    bytes: &[u8],
) -> Result<(), String> {
    offsets.push(file.stream_position().map_err(|e| e.to_string())?);
    write!(
        file,
        "{id} 0 obj\n<< {attributes} /Length {} >>\nstream\n",
        bytes.len()
    )
    .and_then(|_| file.write_all(bytes))
    .and_then(|_| file.write_all(b"\nendstream\nendobj\n"))
    .map_err(|e| e.to_string())
}
