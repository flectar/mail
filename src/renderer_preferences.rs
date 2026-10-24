//! Renderer selection must be available before Slint or the mail databases start.
use crate::{AppWindow, RendererSettings};
use slint::ComponentHandle;
use std::{
    io::{Read, Write},
    path::{Path, PathBuf},
};

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "lowercase")]
pub(crate) enum RendererMode {
    #[default]
    Cpu,
    Gpu,
}

impl RendererMode {
    pub(crate) fn key(self) -> &'static str {
        match self {
            Self::Cpu => "cpu",
            Self::Gpu => "gpu",
        }
    }
    pub(crate) fn parse(value: &str) -> Option<Self> {
        match value {
            "cpu" => Some(Self::Cpu),
            "gpu" => Some(Self::Gpu),
            _ => None,
        }
    }
}

#[derive(Default, serde::Serialize, serde::Deserialize)]
struct Preference {
    #[serde(default)]
    renderer: RendererMode,
}

pub(crate) const GPU_SUPPORTED: bool = cfg!(all(
    feature = "gpu-renderer",
    desktop
));

pub(crate) fn load(path: &Path) -> RendererMode {
    let result = (|| -> Result<Preference, Box<dyn std::error::Error>> {
        let mut bytes = Vec::new();
        std::fs::File::open(path)?
            .take(4097)
            .read_to_end(&mut bytes)?;
        if bytes.len() > 4096 {
            return Err("renderer preference is too large".into());
        }
        Ok(serde_json::from_slice(&bytes)?)
    })();
    match result {
        Ok(preference) => preference.renderer,
        Err(error) => {
            if path.exists() {
                eprintln!("renderer preference unavailable; defaulting to CPU: {error}");
            }
            RendererMode::Cpu
        }
    }
}

fn save(path: &Path, mode: RendererMode) -> Result<(), Box<dyn std::error::Error>> {
    let parent = path
        .parent()
        .ok_or("renderer preference has no parent directory")?;
    std::fs::create_dir_all(parent)?;
    let mut file = tempfile::NamedTempFile::new_in(parent)?;
    serde_json::to_writer(&mut file, &Preference { renderer: mode })?;
    file.write_all(b"\n")?;
    file.as_file().sync_all()?;
    file.persist(path)?;
    Ok(())
}

/// Set on the CPU process that replaces one whose GPU startup failed.
const GPU_FALLBACK: &str = "FLECTAR_GPU_FALLBACK";

pub(crate) fn gpu_startup_failed() -> bool {
    std::env::var_os(GPU_FALLBACK).is_some()
}

pub(crate) fn requested(preferred: RendererMode) -> RendererMode {
    if gpu_startup_failed() {
        return RendererMode::Cpu;
    }
    match std::env::var("FLECTAR_RENDERER") {
        Ok(value) => RendererMode::parse(&value).unwrap_or_else(|| {
            eprintln!("invalid FLECTAR_RENDERER; using saved preference");
            preferred
        }),
        Err(_) => preferred,
    }
}

pub(crate) fn register(
    app: &AppWindow,
    path: PathBuf,
    preferred: RendererMode,
    gpu: bool,
    supported: bool,
    gpu_failed: bool,
) {
    let settings = app.global::<RendererSettings>();
    settings.set_preferred(preferred.key().into());
    settings.set_active(if gpu { "gpu" } else { "cpu" }.into());
    settings.set_gpu_supported(supported);
    settings.set_gpu_failed(gpu_failed);
    settings.set_available(!cfg!(mobile));
    let weak = app.as_weak();
    settings.on_choose(move |value| {
        let Some(app) = weak.upgrade() else {
            return;
        };
        let Some(mode) = RendererMode::parse(&value) else {
            return;
        };
        if mode == RendererMode::Gpu && !supported {
            return;
        }
        let settings = app.global::<RendererSettings>();
        match save(&path, mode) {
            Ok(()) => {
                settings.set_preferred(mode.key().into());
                settings.set_error("".into());
            }
            Err(error) => {
                eprintln!("could not save renderer preference: {error}");
                settings.set_error("Could not save renderer preference. Please try again.".into());
            }
        }
    });
}

#[derive(Debug)]
pub(crate) struct GpuStartupError(pub String);
impl std::fmt::Display for GpuStartupError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}
impl std::error::Error for GpuStartupError {}

pub(crate) fn startup_error(
    error: impl std::fmt::Display,
    gpu: bool,
) -> Box<dyn std::error::Error> {
    if gpu {
        Box::new(GpuStartupError(error.to_string()))
    } else {
        error.to_string().into()
    }
}

pub(crate) fn initialize_step<T>(
    gpu: bool,
    step: impl FnOnce() -> Result<T, slint::PlatformError>,
) -> Result<T, Box<dyn std::error::Error>> {
    if !gpu {
        return step().map_err(|error| startup_error(error, false));
    }
    std::panic::catch_unwind(std::panic::AssertUnwindSafe(step))
        .map_err(|_| startup_error("GPU initialization panicked", true))?
        .map_err(|error| startup_error(error, true))
}

#[cfg(desktop)]
pub(crate) fn select_backend(
    mode: RendererMode,
    window_frame: crate::window_chrome::WindowFrame,
) -> Result<bool, Box<dyn std::error::Error>> {
    let winit = || {
        let selector = slint::BackendSelector::new().backend_name("winit".into());
        #[cfg(any(target_os = "windows", target_os = "macos"))]
        let selector = selector.with_winit_window_attributes_hook(
            crate::window_chrome::window_attributes(window_frame),
        );
        #[cfg(not(any(target_os = "windows", target_os = "macos")))]
        let _ = window_frame;
        selector
    };
    #[cfg(feature = "gpu-renderer")]
    if mode == RendererMode::Gpu {
        let mut settings = slint::wgpu_30::WGPUSettings::default();
        // Normal Vello uses compute/storage buffers, unavailable in WebGL2 limits.
        settings.device_required_limits = slint::wgpu_30::wgpu::Limits::default();
        initialize_step(true, || {
            winit()
                .renderer_name("femtovg-wgpu".into())
                .require_wgpu_30(slint::wgpu_30::WGPUConfiguration::Automatic(settings))
                .select()
        })?;
        return Ok(true);
    }
    if mode == RendererMode::Gpu {
        eprintln!("GPU renderer is unavailable in this build; using CPU");
    }
    // Explicit selection wins over SLINT_BACKEND, including in GPU-capable builds.
    winit().renderer_name("software".into()).select()?;
    Ok(false)
}

#[cfg(desktop)]
pub(crate) fn restart_cpu(error: &dyn std::error::Error) -> Result<(), Box<dyn std::error::Error>> {
    eprintln!("GPU initialization failed; restarting once in CPU / Low Memory mode: {error}");
    let mut command = std::process::Command::new(std::env::current_exe()?);
    // The fallback reopens the window rather than the tray: GPU failures
    // surface while the window is being created or shown, and a hidden
    // restart could leave the user with neither.
    command
        .args(
            std::env::args_os()
                .skip(1)
                .filter(|argument| argument != crate::autostart::BACKGROUND_ARGUMENT),
        )
        .env(GPU_FALLBACK, "1");
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        Err(command.exec().into())
    }
    #[cfg(not(unix))]
    {
        command.spawn()?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn preference_defaults_survives_restart_and_recovers_from_invalid_data() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("renderer.json");
        assert_eq!(load(&path), RendererMode::Cpu);
        save(&path, RendererMode::Gpu).unwrap();
        assert_eq!(load(&path), RendererMode::Gpu);
        save(&path, RendererMode::Cpu).unwrap();
        assert_eq!(load(&path), RendererMode::Cpu);
        for bytes in ["{", "{\"renderer\":\"future\"}", &"x".repeat(4097)] {
            std::fs::write(&path, bytes).unwrap();
            assert_eq!(load(&path), RendererMode::Cpu);
        }
    }
}
