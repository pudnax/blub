use notify::Watcher;
use std::ffi::OsStr;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

pub enum ShaderStage {
    Vertex,
    Fragment,
    #[allow(dead_code)]
    Compute,
}

fn load_shader(glsl_code: &str, identifier: &str, stage: ShaderStage) -> Option<Vec<u32>> {
    let kind = match stage {
        ShaderStage::Vertex => shaderc::ShaderKind::Vertex,
        ShaderStage::Fragment => shaderc::ShaderKind::Fragment,
        ShaderStage::Compute => shaderc::ShaderKind::Compute,
    };

    let mut compiler = shaderc::Compiler::new().unwrap();
    //let mut options = shaderc::CompileOptions::new().unwrap();
    match compiler.compile_into_spirv(glsl_code, kind, identifier, "main", None) {
        Ok(compile_result) => {
            if compile_result.get_num_warnings() > 0 {
                println!("warnings when compiling {}:\n{}", identifier, compile_result.get_warning_messages());
            }

            match wgpu::read_spirv(std::io::Cursor::new(&compile_result.as_binary_u8())) {
                Ok(spirv) => Some(spirv),
                Err(io_error) => {
                    println!("Compilation suceeded, but wgpu::read_spirv failed: {}", io_error);
                    None
                }
            }
        }
        Err(compile_error) => {
            println!("{}", compile_error);
            None
        }
    }
}

pub struct ShaderDirectory {
    #[allow(dead_code)]
    watcher: notify::RecommendedWatcher,
    detected_change: Arc<AtomicBool>,
    directory: PathBuf,
}

impl ShaderDirectory {
    pub fn new(path: &Path) -> ShaderDirectory {
        let detected_change = Arc::new(AtomicBool::new(false));
        let detected_change_evt_ref = detected_change.clone();
        let mut watcher: notify::RecommendedWatcher = notify::Watcher::new_immediate(move |res| match res {
            Ok(_) => detected_change_evt_ref.store(true, Ordering::Relaxed),
            Err(e) => println!("Failed to create filewatcher: {:?}", e),
        })
        .unwrap();
        watcher.watch(path, notify::RecursiveMode::Recursive).unwrap();

        ShaderDirectory {
            watcher,
            detected_change,
            directory: PathBuf::from(path),
        }
    }

    // Checks if any change was detected in the shader directory.
    // Right now notifies any all changes in the directory, if too slow consider filtering & distinguishing shaders.
    pub fn detected_change(&self) -> bool {
        self.detected_change.swap(false, Ordering::Relaxed)
    }

    pub fn load_shader_module(&self, device: &wgpu::Device, relative_filename: &Path) -> Option<wgpu::ShaderModule> {
        let path = self.directory.join(relative_filename);

        let shader_stage = match path.extension().and_then(OsStr::to_str) {
            Some("frag") => ShaderStage::Fragment,
            Some("vert") => ShaderStage::Vertex,
            _ => {
                println!("Did not recognize file extension for shader file \"{:?}\"", path);
                return None;
            }
        };

        match std::fs::read_to_string(&path) {
            Ok(glsl_code) => match load_shader(&glsl_code, &relative_filename.to_str().unwrap(), shader_stage) {
                Some(spirv) => Some(device.create_shader_module(&spirv)),
                None => None,
            },
            Err(err) => {
                println!("Failed to read shader file \"{:?}\": {}", path, err);
                None
            }
        }
    }
}