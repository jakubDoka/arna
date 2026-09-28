use {
    super::DynamicLibrary,
    crate::{TEMP_ARENAS, TempArenas},
    core::ffi::c_void,
    std::{
        collections::BTreeSet,
        ffi::OsString,
        fmt, fs, io, mem,
        path::{Path, PathBuf},
        process::{Command, ExitStatus, Stdio},
        sync::atomic::{AtomicPtr, Ordering},
        time::SystemTime,
    },
};

type Constructor = unsafe extern "C" fn() -> *mut c_void;
type Destructor = unsafe extern "C" fn(*mut c_void);
type StateVersion = unsafe extern "C" fn() -> usize;
type StateFunction = unsafe extern "C" fn(*mut c_void);

#[derive(Debug)]
pub enum Error {
    MissingSymbolName(&'static str),
    InvalidSymbolName,
    UnsupportedCargoArguments(&'static str),
    Cargo(io::Error),
    CargoFailed(ExitStatus),
    ArtifactNotFound,
    MultipleArtifacts(Vec<PathBuf>),
    DependencyFileNotFound(PathBuf),
    Io(io::Error),
    DynamicLibrary(String),
    Symbol { name: String, error: String },
    NotLoaded,
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::MissingSymbolName(kind) => {
                write!(f, "the {kind} symbol name was not configured")
            }
            Self::InvalidSymbolName => {
                write!(f, "symbol names cannot contain NUL bytes")
            }
            Self::UnsupportedCargoArguments(argument) => write!(
                f,
                "the hot module controls `{argument}` and it cannot be passed in the Cargo arguments"
            ),
            Self::Cargo(error) => write!(f, "failed to run Cargo: {error}"),
            Self::CargoFailed(status) => {
                write!(f, "Cargo failed with {status}")
            }
            Self::ArtifactNotFound => {
                write!(f, "Cargo did not produce a cdylib artifact")
            }
            Self::MultipleArtifacts(paths) => write!(
                f,
                "Cargo produced multiple cdylib artifacts: {}",
                paths
                    .iter()
                    .map(|path| path.display().to_string())
                    .collect::<Vec<_>>()
                    .join(", ")
            ),
            Self::DependencyFileNotFound(path) => write!(
                f,
                "Cargo did not produce the expected dependency file `{}`",
                path.display()
            ),
            Self::Io(error) => error.fmt(f),
            Self::DynamicLibrary(error) => {
                write!(f, "failed to load dynamic library: {error}")
            }
            Self::Symbol { name, error } => {
                write!(f, "failed to load symbol `{name}`: {error}")
            }
            Self::NotLoaded => {
                write!(f, "the hot module has not been loaded yet")
            }
        }
    }
}

impl std::error::Error for Error {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Cargo(error) | Self::Io(error) => Some(error),
            _ => None,
        }
    }
}

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

    /// Builds and reloads the module if one of its Cargo inputs changed.
    ///
    /// Returns `true` when a new dynamic library was loaded. This is the only
    /// operation that checks the watched files or invokes Cargo.
    ///
    /// # Safety
    ///
    /// The configured symbols must have the following ABIs:
    ///
    /// - constructor: `unsafe extern "C" fn() -> *mut c_void`
    /// - destructor: `unsafe extern "C" fn(*mut c_void)`
    /// - state version: `unsafe extern "C" fn() -> usize`
    ///
    /// If the library exports Arna's `ARNA_TEMP_ARENAS_OVERRIDE`, it must be
    /// the `AtomicPtr<TempArenas>` provided by Arna's `dll` feature.
    ///
    /// A state version must only be reused when functions in the newly built
    /// library can safely operate on state created by an older library with
    /// that version. None of the functions may unwind across the ABI boundary.
    pub unsafe fn reload_if_changed(&mut self) {
        pub unsafe fn perform(slf: &mut Module) -> Result<(), Error> {
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
            fs::copy(&artifact.library, &hot_path).map_err(Error::Io)?;

            let loaded = match slf.load(&hot_path, &artifact) {
                Ok(loaded) => loaded,
                Err(error) => {
                    let _ = fs::remove_file(&hot_path);
                    return Err(error);
                }
            };

            let version = unsafe { (loaded.state_version)() };
            if slf.state.as_ref().map(|state| state.version) != Some(version) {
                if let Some(state) = slf.state.take() {
                    unsafe { (state.destructor)(state.pointer) };
                }
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

    /// Calls `unsafe extern "C" fn(*mut c_void)` from the current library.
    ///
    /// # Safety
    ///
    /// `name` must identify a function with that exact ABI, and the function
    /// must accept the state created by the configured constructor. It must not
    /// retain the pointer beyond the call or unwind across the ABI boundary.
    pub unsafe fn call(&mut self, name: &str) -> Result<(), Error> {
        if name.as_bytes().contains(&0) {
            return Err(Error::InvalidSymbolName);
        }

        let library = self
            .current_library
            .and_then(|index| self.libraries.get(index))
            .ok_or(Error::NotLoaded)?;
        let state = self.state.as_mut().ok_or(Error::NotLoaded)?;
        let pointer = unsafe { library.symbol::<c_void>(name) }
            .map_err(|error| Error::Symbol { name: name.to_owned(), error })?;
        let function =
            unsafe { mem::transmute::<*mut c_void, StateFunction>(pointer) };
        unsafe { function(state.pointer) };
        Ok(())
    }

    fn build_artifact(&self) -> Result<Artifact, Error> {
        let mut command = Command::new("cargo");
        if let Some(separator) =
            self.cargo_args.iter().position(|argument| argument == "--")
        {
            command
                .args(&self.cargo_args[..separator])
                .arg("--message-format=json-render-diagnostics")
                .args(&self.cargo_args[separator..]);
        } else {
            command
                .args(&self.cargo_args)
                .arg("--message-format=json-render-diagnostics");
        }
        let output =
            command.stderr(Stdio::inherit()).output().map_err(Error::Cargo)?;

        let stdout = String::from_utf8_lossy(&output.stdout);
        let mut artifacts = BTreeSet::new();
        for line in stdout.lines() {
            if line.contains("\"reason\":\"compiler-message\"")
                && let Some(rendered) = json_string_field(line, "rendered")
            {
                eprint!("{rendered}");
            }

            if !line.contains("\"reason\":\"compiler-artifact\"")
                || !json_array_contains(line, "crate_types", "cdylib")
            {
                continue;
            }

            let Some(library) = json_string_array(line, "filenames")
                .into_iter()
                .map(PathBuf::from)
                .find(|path| {
                    path.to_string_lossy()
                        .ends_with(std::env::consts::DLL_SUFFIX)
                })
            else {
                continue;
            };
            let manifest =
                json_string_field(line, "manifest_path").map(PathBuf::from);
            artifacts.insert((library, manifest));
        }

        if !output.status.success() {
            return Err(Error::CargoFailed(output.status));
        }

        match artifacts.len() {
            0 => Err(Error::ArtifactNotFound),
            1 => {
                let (library, manifest) = artifacts.into_iter().next().unwrap();
                Ok(Artifact { library, manifest })
            }
            _ => Err(Error::MultipleArtifacts(
                artifacts.into_iter().map(|(path, _)| path).collect(),
            )),
        }
    }

    fn hot_path(&mut self, artifact: &Path) -> Result<PathBuf, Error> {
        let stem = artifact.file_stem().ok_or_else(|| {
            Error::Io(io::Error::other("cdylib artifact has no file stem"))
        })?;
        let extension = artifact.extension().ok_or_else(|| {
            Error::Io(io::Error::other("cdylib artifact has no extension"))
        })?;
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

    fn load(&self, path: &Path, artifact: &Artifact) -> Result<Loaded, Error> {
        let library =
            DynamicLibrary::open(Some(path)).map_err(Error::DynamicLibrary)?;
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
    ) -> Result<T, Error> {
        let pointer = unsafe { library.symbol::<c_void>(name) }
            .map_err(|error| Error::Symbol { name: name.to_owned(), error })?;
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

impl Drop for Module {
    fn drop(&mut self) {
        if let Some(state) = self.state.take() {
            unsafe { (state.destructor)(state.pointer) };
        }
        self.libraries.clear();
        for path in &self.library_paths {
            let _ = fs::remove_file(path);
        }
    }
}

struct Artifact {
    library: PathBuf,
    manifest: Option<PathBuf>,
}

struct Loaded {
    library: DynamicLibrary,
    constructor: Constructor,
    destructor: Destructor,
    state_version: StateVersion,
    watched_files: BTreeSet<PathBuf>,
}

fn watched_files(artifact: &Artifact) -> Result<BTreeSet<PathBuf>, Error> {
    let dependency_file = artifact.library.with_extension("d");
    let contents = fs::read_to_string(&dependency_file).map_err(|error| {
        if error.kind() == io::ErrorKind::NotFound {
            Error::DependencyFileNotFound(dependency_file.clone())
        } else {
            Error::Io(error)
        }
    })?;
    let base = std::env::current_dir().map_err(Error::Io)?;
    let mut files = parse_dependency_file(&contents)
        .into_iter()
        .map(|path| if path.is_absolute() { path } else { base.join(path) })
        .collect::<BTreeSet<_>>();

    if let Some(manifest) = &artifact.manifest {
        files.insert(manifest.clone());
        if let Some(lock) = manifest.parent().and_then(|directory| {
            directory
                .ancestors()
                .map(|path| path.join("Cargo.lock"))
                .find(|path| path.is_file())
        }) {
            files.insert(lock);
        }
    }

    Ok(files)
}

fn parse_dependency_file(contents: &str) -> Vec<PathBuf> {
    let Some(mut index) = contents
        .as_bytes()
        .windows(2)
        .position(|pair| pair[0] == b':' && pair[1].is_ascii_whitespace())
        .map(|index| index + 2)
    else {
        return Vec::new();
    };

    let bytes = contents.as_bytes();
    let mut paths = Vec::new();
    let mut path = Vec::new();
    while index < bytes.len() {
        match bytes[index] {
            b'\\'
                if bytes.get(index + 1) == Some(&b'\r')
                    && bytes.get(index + 2) == Some(&b'\n') =>
            {
                index += 3
            }
            b'\\' if bytes.get(index + 1) == Some(&b'\n') => index += 2,
            b'\\' if index + 1 < bytes.len() => {
                path.push(bytes[index + 1]);
                index += 2;
            }
            byte if byte.is_ascii_whitespace() => {
                if !path.is_empty() {
                    paths.push(PathBuf::from(
                        String::from_utf8_lossy(&path).into_owned(),
                    ));
                    path.clear();
                }
                index += 1;
            }
            byte => {
                path.push(byte);
                index += 1;
            }
        }
    }
    if !path.is_empty() {
        paths.push(PathBuf::from(String::from_utf8_lossy(&path).into_owned()));
    }
    paths
}

fn json_array_contains(input: &str, field: &str, expected: &str) -> bool {
    json_string_array(input, field).iter().any(|value| value == expected)
}

fn json_string_array(input: &str, field: &str) -> Vec<String> {
    let Some(mut index) = json_field_value(input, field) else {
        return Vec::new();
    };
    let bytes = input.as_bytes();
    if bytes.get(index) != Some(&b'[') {
        return Vec::new();
    }
    index += 1;

    let mut values = Vec::new();
    while index < bytes.len() {
        skip_json_whitespace(bytes, &mut index);
        match bytes.get(index) {
            Some(b']') | None => break,
            Some(b'"') => {
                if let Some(value) = parse_json_string(input, &mut index) {
                    values.push(value);
                } else {
                    break;
                }
            }
            _ => break,
        }
        skip_json_whitespace(bytes, &mut index);
        if bytes.get(index) == Some(&b',') {
            index += 1;
        }
    }
    values
}

fn json_string_field(input: &str, field: &str) -> Option<String> {
    let mut index = json_field_value(input, field)?;
    parse_json_string(input, &mut index)
}

fn json_field_value(input: &str, field: &str) -> Option<usize> {
    let needle = format!("\"{field}\"");
    let bytes = input.as_bytes();
    let mut index = input.find(&needle)? + needle.len();
    skip_json_whitespace(bytes, &mut index);
    if bytes.get(index) != Some(&b':') {
        return None;
    }
    index += 1;
    skip_json_whitespace(bytes, &mut index);
    Some(index)
}

fn skip_json_whitespace(bytes: &[u8], index: &mut usize) {
    while bytes.get(*index).is_some_and(u8::is_ascii_whitespace) {
        *index += 1;
    }
}

fn parse_json_string(input: &str, index: &mut usize) -> Option<String> {
    let bytes = input.as_bytes();
    if bytes.get(*index) != Some(&b'"') {
        return None;
    }
    *index += 1;
    let mut value = String::new();
    let mut plain_start = *index;

    while *index < bytes.len() {
        match bytes[*index] {
            b'"' => {
                value.push_str(input.get(plain_start..*index)?);
                *index += 1;
                return Some(value);
            }
            b'\\' => {
                value.push_str(input.get(plain_start..*index)?);
                *index += 1;
                let escaped = *bytes.get(*index)?;
                *index += 1;
                match escaped {
                    b'"' => value.push('"'),
                    b'\\' => value.push('\\'),
                    b'/' => value.push('/'),
                    b'b' => value.push('\u{8}'),
                    b'f' => value.push('\u{c}'),
                    b'n' => value.push('\n'),
                    b'r' => value.push('\r'),
                    b't' => value.push('\t'),
                    b'u' => {
                        let hex = input.get(*index..*index + 4)?;
                        let code = u32::from_str_radix(hex, 16).ok()?;
                        value.push(char::from_u32(code)?);
                        *index += 4;
                    }
                    _ => return None,
                }
                plain_start = *index;
            }
            _ => *index += 1,
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use {
        super::{json_string_array, json_string_field, parse_dependency_file},
        std::path::PathBuf,
    };

    #[test]
    fn parses_cargo_json_strings() {
        let json = r#"{"manifest_path":"C:\\work\\Cargo.toml","filenames":["/tmp/lib one.so","/tmp/lib.rlib"]}"#;
        assert_eq!(
            json_string_field(json, "manifest_path").unwrap(),
            "C:\\work\\Cargo.toml"
        );
        assert_eq!(
            json_string_array(json, "filenames"),
            ["/tmp/lib one.so", "/tmp/lib.rlib"]
        );
    }

    #[test]
    fn parses_escaped_dependency_paths() {
        let paths = parse_dependency_file(
            "target/lib.so: src/lib.rs path\\ with\\ spaces.rs \\\r\nsrc/other.rs \\\nsrc/final.rs\n",
        );
        assert_eq!(
            paths,
            [
                PathBuf::from("src/lib.rs"),
                PathBuf::from("path with spaces.rs"),
                PathBuf::from("src/other.rs"),
                PathBuf::from("src/final.rs"),
            ]
        );
    }
}
