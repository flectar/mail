use flectar_mail::favicon::{FaviconLoader, domain_from_address};
use image::{ColorType, ImageFormat};
use std::{env, fs, path::PathBuf};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let mut arguments = env::args_os().skip(1);
    let output_dir = arguments
        .next()
        .map(PathBuf::from)
        .ok_or("usage: generate-screenshot-favicons OUTPUT_DIR ADDRESS...")?;
    let addresses: Vec<_> = arguments.collect();
    if addresses.is_empty() {
        return Err("at least one sender address is required".into());
    }

    fs::create_dir_all(&output_dir)?;
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()?;
    let cache_dir = tempfile::tempdir()?;
    let loader = FaviconLoader::new(cache_dir.path())?;

    for address in addresses {
        let address = address.to_string_lossy();
        let domain = domain_from_address(&address)
            .ok_or_else(|| format!("invalid sender address: {address}"))?;
        let output = output_dir.join(format!("{domain}.png"));
        if output.exists() {
            continue;
        }
        let icons = runtime
            .block_on(loader.load(&domain, 76, 76))?
            .ok_or_else(|| format!("no favicon found for {domain}"))?;
        let icon = icons.regular;
        image::save_buffer_with_format(
            output,
            &icon.pixels,
            icon.width,
            icon.height,
            ColorType::Rgba8,
            ImageFormat::Png,
        )?;
    }

    Ok(())
}
