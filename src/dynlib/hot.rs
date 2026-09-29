use {
    super::DynamicLibrary,
    crate::{TEMP_ARENAS, TempArenas},
    core::ffi::c_void,
    std::{
        collections::BTreeSet,
        ffi::{OsStr, OsString},
        fs, io, mem,
        path::{Path, PathBuf},
        process::Command,
        sync::atomic::{AtomicPtr, Ordering},
        time::SystemTime,
    },
};

/// Small utility to quickly export the hot reloaded object api
#[macro_export]
macro_rules! hot_api {
    // TODO: maybe generate helpers for making the module that creates this
    (
        impl Hot for $ty:ty {
            fn $drop:ident() -> Self;

            fn $create:ident() -> Self {
                $($create_compute:tt)*
            }

            fn $version:ident() -> usize {
                $($version_compute:tt)*
            }

            $(
                fn $fn_name:ident($slf:ident: &mut Self) {
                    $($fn_compute:tt)*
                }
            )*
        }
    ) => {
        #[unsafe(no_mangle)]
        extern "C" fn $create() -> *mut c_void {
            Box::into_raw(Box::new($($create_compute)*)).cast()
        }

        #[unsafe(no_mangle)]
        unsafe extern "C" fn $drop(state: *mut c_void) {
            drop(unsafe { Box::from_raw(state.cast::<$ty>()) });
        }

        #[unsafe(no_mangle)]
        extern "C" fn $version() -> usize {
            $($version_compute)*
        }

        $(
            #[unsafe(no_mangle)]
            unsafe extern "C" fn $fn_name(state: *mut c_void) {
                let $slf = unsafe { &mut *state.cast::<$ty>() };
                $($fn_compute)*
            }
        )*
    };
}

type Constructor = unsafe extern "C" fn() -> *mut c_void;
type Destructor = unsafe extern "C" fn(*mut c_void);
type StateVersion = unsafe extern "C" fn() -> usize;
type StateFunction = unsafe extern "C" fn(*mut c_void);

struct State {
    pointer: *mut c_void,
    destructor: Destructor,
    version: usize,
}

pub struct Module {
    cargo_args: Vec<OsString>,
    constructor: String,
    destructor: String,
    state_version: String,
    state: Option<State>,
    current_library: Option<usize>,
    libraries: Vec<DynamicLibrary>,
    library_paths: Vec<PathBuf>,
    watched_files: Vec<PathBuf>,
    last_change: SystemTime,
    next_library_id: usize,
}

impl Module {
    pub fn new<I, S>(
        constructor: impl Into<String>,
        destructor: impl Into<String>,
        state_version: impl Into<String>,
        cargo_args: I,
    ) -> Self
    where
        I: IntoIterator<Item = S>,
        S: Into<OsString>,
    {
        Self {
            cargo_args: cargo_args.into_iter().map(Into::into).collect(),
            constructor: constructor.into(),
            destructor: destructor.into(),
            state_version: state_version.into(),
            state: None,
            current_library: None,
            libraries: Vec::new(),
            library_paths: Vec::new(),
            watched_files: Vec::new(),
            next_library_id: 0,
            last_change: SystemTime::UNIX_EPOCH,
        }
    }

    pub unsafe fn reload_if_changed(&mut self) {
        pub unsafe fn perform(slf: &mut Module) -> Result<(), String> {
            let latest = slf
                .watched_files
                .iter()
                .map(|path| {
                    path.metadata()
                        .and_then(|v| v.modified())
                        .unwrap_or(SystemTime::UNIX_EPOCH)
                })
                .max()
                .unwrap_or(SystemTime::UNIX_EPOCH);

            if latest <= slf.last_change && !slf.watched_files.is_empty() {
                return Ok(());
            }

            slf.last_change = latest;

            let artifact = slf.build_artifact()?;
            let hot_path = slf.hot_path(&artifact.library)?;
            fs::copy(&artifact.library, &hot_path)
                .map_err(|error| error.to_string())?;

            let loaded = match slf.load(&hot_path, &artifact) {
                Ok(loaded) => loaded,
                Err(error) => {
                    let _ = fs::remove_file(&hot_path);
                    return Err(error);
                }
            };

            let version = unsafe { (loaded.state_version)() };
            if slf.state.as_ref().map(|state| state.version) != Some(version) {
                slf.state = Some(State {
                    pointer: unsafe { (loaded.constructor)() },
                    destructor: loaded.destructor,
                    version,
                });
            } else if let Some(state) = &mut slf.state {
                state.destructor = loaded.destructor;
            }

            slf.watched_files = loaded.watched_files.into_iter().collect();
            slf.current_library = Some(slf.libraries.len());
            slf.libraries.push(loaded.library);
            slf.library_paths.push(hot_path);
            Ok(())
        }

        if let Err(e) = unsafe { perform(self) } {
            eprintln!("{e:#}");
        }
    }

    pub unsafe fn call(&mut self, name: &str) -> Result<(), String> {
        if name.as_bytes().contains(&0) {
            return Err("symbol names cannot contain NUL bytes".to_owned());
        }

        let library = self
            .current_library
            .and_then(|index| self.libraries.get(index))
            .ok_or_else(|| {
                "the hot module has not been loaded yet".to_owned()
            })?;
        let state = self.state.as_mut().ok_or_else(|| {
            "the hot module has not been loaded yet".to_owned()
        })?;
        let pointer =
            unsafe { library.symbol::<c_void>(name) }.map_err(|error| {
                format!("failed to load symbol `{name}`: {error}")
            })?;
        let function =
            unsafe { mem::transmute::<*mut c_void, StateFunction>(pointer) };
        unsafe { function(state.pointer) };
        Ok(())
    }

    fn build_artifact(&self) -> Result<Artifact, String> {
        let artifact = artifact_from_cargo_args(&self.cargo_args)?;
        let status = Command::new("cargo")
            .args(&self.cargo_args)
            .status()
            .map_err(|error| format!("failed to run Cargo: {error}"))?;
        if !status.success() {
            return Err(format!("Cargo failed with {status}"));
        }

        Ok(artifact)
    }

    fn hot_path(&mut self, artifact: &Path) -> Result<PathBuf, String> {
        let stem = artifact
            .file_stem()
            .ok_or_else(|| "cdylib artifact has no file stem".to_owned())?;
        let extension = artifact
            .extension()
            .ok_or_else(|| "cdylib artifact has no extension".to_owned())?;
        let path = artifact.with_file_name(format!(
            "{}.hot-{}-{}.{}",
            stem.to_string_lossy(),
            std::process::id(),
            self.next_library_id,
            extension.to_string_lossy(),
        ));
        self.next_library_id += 1;
        Ok(path)
    }

    fn load(&self, path: &Path, artifact: &Artifact) -> Result<Loaded, String> {
        let library = DynamicLibrary::open(Some(path)).map_err(|error| {
            format!("failed to load dynamic library: {error}")
        })?;
        unsafe { connect_temp_arenas(&library) };
        let constructor =
            unsafe { self.symbol::<Constructor>(&library, &self.constructor)? };
        let destructor =
            unsafe { self.symbol::<Destructor>(&library, &self.destructor)? };
        let state_version = unsafe {
            self.symbol::<StateVersion>(&library, &self.state_version)?
        };
        let watched_files = watched_files(artifact)?;

        Ok(Loaded {
            library,
            constructor,
            destructor,
            state_version,
            watched_files,
        })
    }

    unsafe fn symbol<T: Copy>(
        &self,
        library: &DynamicLibrary,
        name: &str,
    ) -> Result<T, String> {
        let pointer =
            unsafe { library.symbol::<c_void>(name) }.map_err(|error| {
                format!("failed to load symbol `{name}`: {error}")
            })?;
        debug_assert_eq!(mem::size_of::<T>(), mem::size_of_val(&pointer));
        Ok(unsafe { mem::transmute_copy(&pointer) })
    }
}

unsafe fn connect_temp_arenas(library: &DynamicLibrary) {
    let Ok(slot) = (unsafe {
        library.symbol::<AtomicPtr<TempArenas>>("ARNA_TEMP_ARENAS_OVERRIDE")
    }) else {
        return;
    };
    let Some(slot) = (unsafe { slot.as_ref() }) else {
        return;
    };
    slot.store(
        &TEMP_ARENAS as *const TempArenas as *mut TempArenas,
        Ordering::Relaxed,
    );
}

impl Drop for State {
    fn drop(&mut self) {
        unsafe { (self.destructor)(self.pointer) };
    }
}

impl Drop for Module {
    fn drop(&mut self) {
        self.state.take();
        self.libraries.clear();
        for path in &self.library_paths {
            let _ = fs::remove_file(path);
        }
    }
}

struct Artifact {
    library: PathBuf,
    dependency_file: PathBuf,
}

struct Loaded {
    library: DynamicLibrary,
    constructor: Constructor,
    destructor: Destructor,
    state_version: StateVersion,
    watched_files: BTreeSet<PathBuf>,
}

fn watched_files(artifact: &Artifact) -> Result<BTreeSet<PathBuf>, String> {
    let contents =
        fs::read_to_string(&artifact.dependency_file).map_err(|error| {
            if error.kind() == io::ErrorKind::NotFound {
                format!(
                    "Cargo did not produce the expected dependency file `{}`",
                    artifact.dependency_file.display()
                )
            } else {
                error.to_string()
            }
        })?;
    let base = std::env::current_dir().map_err(|error| error.to_string())?;
    let files = parse_dependency_file(&contents)
        .into_iter()
        .map(|path| if path.is_absolute() { path } else { base.join(path) })
        .collect::<BTreeSet<_>>();
    Ok(files)
}

fn artifact_from_cargo_args(args: &[OsString]) -> Result<Artifact, String> {
    let example = cargo_argument(args, "--example")
        .ok_or_else(|| {
            "the Cargo arguments must include `--example <name>`".to_owned()
        })?
        .to_string_lossy()
        .replace('-', "_");
    let profile =
        if cargo_flag(args, "--release") { "release" } else { "debug" };
    let directory = Path::new("target").join(profile).join("examples");
    let stem = format!("{}{example}", std::env::consts::DLL_PREFIX);

    Ok(Artifact {
        library: directory
            .join(format!("{stem}{}", std::env::consts::DLL_SUFFIX)),
        dependency_file: directory.join(format!("{stem}.d")),
    })
}

fn cargo_argument<'a>(args: &'a [OsString], name: &str) -> Option<&'a OsStr> {
    let mut args = args.iter().map(OsString::as_os_str);
    while let Some(argument) = args.next() {
        if argument == "--" {
            break;
        }
        if argument == name {
            return args.next();
        }
        if let Some(value) = argument
            .to_str()
            .and_then(|argument| argument.strip_prefix(name))
            .and_then(|argument| argument.strip_prefix('='))
        {
            return Some(OsStr::new(value));
        }
    }
    None
}

fn cargo_flag(args: &[OsString], name: &str) -> bool {
    args.iter()
        .take_while(|argument| argument.as_os_str() != "--")
        .any(|argument| argument == name)
}

fn parse_dependency_file(contents: &str) -> Vec<PathBuf> {
    contents.split_whitespace().skip(1).map(|s| PathBuf::from(s)).collect()
}
