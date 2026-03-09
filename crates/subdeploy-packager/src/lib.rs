use std::collections::BTreeSet;
use std::fs::{self, File};
use std::path::{Path, PathBuf};

use flate2::write::GzEncoder;
use flate2::Compression;
use ignore::WalkBuilder;
use tar::Builder;
use thiserror::Error;

#[derive(Debug, Clone)]
pub struct PackageRequest {
    pub project_dir: PathBuf,
    pub dockerfile: Option<PathBuf>,
    pub compose_file: Option<PathBuf>,
}

#[derive(Debug, Clone)]
pub struct ProjectInspection {
    pub project_name: String,
    pub project_dir: PathBuf,
    pub dockerfile_rel: PathBuf,
    pub compose_file_rel: PathBuf,
    pub included_files: Vec<PathBuf>,
}

#[derive(Debug, Clone)]
pub struct ProjectPackage {
    pub project_name: String,
    pub archive_path: PathBuf,
    pub dockerfile_rel: PathBuf,
    pub compose_file_rel: PathBuf,
    pub included_files: Vec<PathBuf>,
}

#[derive(Debug, Error)]
pub enum PackageError {
    #[error("项目目录不存在或不是目录: {0}")]
    InvalidProjectDir(PathBuf),
    #[error("部署文件不存在: {0}")]
    MissingRequiredFile(PathBuf),
    #[error("缺少 compose 文件，支持 docker-compose.yml 或 compose.yml")]
    MissingComposeFile,
    #[error("部署文件被 .gitignore 规则排除: {0}")]
    RequiredFileIgnored(PathBuf),
    #[error("文件路径不在项目目录内: {0}")]
    PathOutsideProject(PathBuf),
    #[error("未找到可归档的项目文件")]
    EmptyPackage,
    #[error("创建输出目录失败: {0}")]
    CreateOutputDir(PathBuf),
    #[error("扫描项目文件失败: {0}")]
    Ignore(#[from] ignore::Error),
    #[error("IO 错误: {0}")]
    Io(#[from] std::io::Error),
}

pub fn inspect_project(request: &PackageRequest) -> Result<ProjectInspection, PackageError> {
    let project_dir = fs::canonicalize(&request.project_dir)
        .map_err(|_| PackageError::InvalidProjectDir(request.project_dir.clone()))?;
    if !project_dir.is_dir() {
        return Err(PackageError::InvalidProjectDir(project_dir));
    }

    let dockerfile_rel = resolve_dockerfile(&project_dir, request.dockerfile.as_ref())?;
    let compose_file_rel = resolve_compose_file(&project_dir, request.compose_file.as_ref())?;
    let mut included_files = collect_files(&project_dir)?;

    let required_paths = [dockerfile_rel.clone(), compose_file_rel.clone()];
    for required in required_paths {
        if !included_files.iter().any(|path| path == &required) {
            return Err(PackageError::RequiredFileIgnored(required));
        }
    }

    included_files.sort();
    let project_name = project_dir
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or("project")
        .to_owned();

    Ok(ProjectInspection {
        project_name,
        project_dir,
        dockerfile_rel,
        compose_file_rel,
        included_files,
    })
}

pub fn package_project(
    request: &PackageRequest,
    output_path: &Path,
) -> Result<ProjectPackage, PackageError> {
    let inspection = inspect_project(request)?;
    let mut files = inspection.included_files.clone();

    if let Some(output_rel) = path_relative_to(output_path, &inspection.project_dir)? {
        files.retain(|path| path != &output_rel);
    }

    if files.is_empty() {
        return Err(PackageError::EmptyPackage);
    }

    if let Some(parent) = output_path.parent() {
        fs::create_dir_all(parent)
            .map_err(|_| PackageError::CreateOutputDir(parent.to_path_buf()))?;
    }

    let output_file = File::create(output_path)?;
    let encoder = GzEncoder::new(output_file, Compression::default());
    let mut builder = Builder::new(encoder);

    for rel_path in &files {
        let full_path = inspection.project_dir.join(rel_path);
        builder.append_path_with_name(&full_path, rel_path)?;
    }

    builder.finish()?;

    Ok(ProjectPackage {
        project_name: inspection.project_name,
        archive_path: output_path.to_path_buf(),
        dockerfile_rel: inspection.dockerfile_rel,
        compose_file_rel: inspection.compose_file_rel,
        included_files: files,
    })
}

fn collect_files(project_dir: &Path) -> Result<Vec<PathBuf>, PackageError> {
    let mut seen = BTreeSet::new();
    let mut builder = WalkBuilder::new(project_dir);
    builder
        .hidden(false)
        .ignore(false)
        .require_git(false)
        .git_global(false)
        .git_exclude(false)
        .parents(true)
        .sort_by_file_path(|a, b| a.cmp(b));

    for entry in builder.build() {
        let entry = entry?;
        let path = entry.path();

        if path == project_dir {
            continue;
        }

        let rel_path = match path.strip_prefix(project_dir) {
            Ok(value) => value,
            Err(_) => continue,
        };

        if rel_path
            .components()
            .next()
            .map(|component| component.as_os_str() == ".git")
            .unwrap_or(false)
        {
            continue;
        }

        if entry.file_type().is_some_and(|kind| kind.is_file()) {
            seen.insert(rel_path.to_path_buf());
        }
    }

    if seen.is_empty() {
        return Err(PackageError::EmptyPackage);
    }

    Ok(seen.into_iter().collect())
}

fn resolve_dockerfile(
    project_dir: &Path,
    dockerfile: Option<&PathBuf>,
) -> Result<PathBuf, PackageError> {
    let candidate = dockerfile
        .cloned()
        .unwrap_or_else(|| PathBuf::from("Dockerfile"));
    resolve_required_file(project_dir, &candidate)
}

fn resolve_compose_file(
    project_dir: &Path,
    compose_file: Option<&PathBuf>,
) -> Result<PathBuf, PackageError> {
    if let Some(candidate) = compose_file {
        return resolve_required_file(project_dir, candidate);
    }

    for candidate in ["docker-compose.yml", "compose.yml"] {
        let rel_path = PathBuf::from(candidate);
        if project_dir.join(&rel_path).is_file() {
            return Ok(rel_path);
        }
    }

    Err(PackageError::MissingComposeFile)
}

fn resolve_required_file(project_dir: &Path, path: &Path) -> Result<PathBuf, PackageError> {
    let full_path = if path.is_absolute() {
        path.to_path_buf()
    } else {
        project_dir.join(path)
    };

    let canonical =
        fs::canonicalize(&full_path).map_err(|_| PackageError::MissingRequiredFile(path.into()))?;
    if !canonical.is_file() {
        return Err(PackageError::MissingRequiredFile(path.into()));
    }

    canonical
        .strip_prefix(project_dir)
        .map(|path| path.to_path_buf())
        .map_err(|_| PackageError::PathOutsideProject(canonical))
}

fn path_relative_to(path: &Path, root: &Path) -> Result<Option<PathBuf>, PackageError> {
    let full_path = if path.is_absolute() {
        path.to_path_buf()
    } else {
        std::env::current_dir()?.join(path)
    };

    match full_path.strip_prefix(root) {
        Ok(value) => Ok(Some(value.to_path_buf())),
        Err(_) => Ok(None),
    }
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::path::Path;

    use tempfile::tempdir;

    use super::{inspect_project, package_project, PackageError, PackageRequest};

    #[test]
    fn inspect_project_respects_gitignore() {
        let dir = tempdir().unwrap();
        fs::write(dir.path().join(".gitignore"), "ignored.txt\n").unwrap();
        fs::write(dir.path().join("Dockerfile"), "FROM scratch\n").unwrap();
        fs::write(dir.path().join("docker-compose.yml"), "services: {}\n").unwrap();
        fs::write(dir.path().join("kept.txt"), "ok\n").unwrap();
        fs::write(dir.path().join("ignored.txt"), "no\n").unwrap();

        let inspection = inspect_project(&PackageRequest {
            project_dir: dir.path().to_path_buf(),
            dockerfile: None,
            compose_file: None,
        })
        .unwrap();

        assert!(inspection
            .included_files
            .iter()
            .any(|path| path == Path::new("kept.txt")));
        assert!(!inspection
            .included_files
            .iter()
            .any(|path| path == Path::new("ignored.txt")));
    }

    #[test]
    fn inspect_project_keeps_dot_files() {
        let dir = tempdir().unwrap();
        fs::write(dir.path().join("Dockerfile"), "FROM scratch\n").unwrap();
        fs::write(dir.path().join("docker-compose.yml"), "services: {}\n").unwrap();
        fs::write(dir.path().join(".env.example"), "DEMO=1\n").unwrap();

        let inspection = inspect_project(&PackageRequest {
            project_dir: dir.path().to_path_buf(),
            dockerfile: None,
            compose_file: None,
        })
        .unwrap();

        assert!(inspection
            .included_files
            .iter()
            .any(|path| path == Path::new(".env.example")));
    }

    #[test]
    fn inspect_project_requires_dockerfile() {
        let dir = tempdir().unwrap();
        fs::write(dir.path().join("docker-compose.yml"), "services: {}\n").unwrap();

        let error = inspect_project(&PackageRequest {
            project_dir: dir.path().to_path_buf(),
            dockerfile: None,
            compose_file: None,
        })
        .unwrap_err();

        assert!(matches!(error, PackageError::MissingRequiredFile(_)));
    }

    #[test]
    fn inspect_project_requires_compose_file() {
        let dir = tempdir().unwrap();
        fs::write(dir.path().join("Dockerfile"), "FROM scratch\n").unwrap();

        let error = inspect_project(&PackageRequest {
            project_dir: dir.path().to_path_buf(),
            dockerfile: None,
            compose_file: None,
        })
        .unwrap_err();

        assert!(matches!(error, PackageError::MissingComposeFile));
    }

    #[test]
    fn package_project_excludes_existing_output_file_inside_project() {
        let dir = tempdir().unwrap();
        fs::write(dir.path().join("Dockerfile"), "FROM scratch\n").unwrap();
        fs::write(dir.path().join("docker-compose.yml"), "services: {}\n").unwrap();
        fs::write(dir.path().join("app.txt"), "hello\n").unwrap();

        let output = dir.path().join("bundle.tar.gz");
        fs::write(&output, "placeholder").unwrap();

        let package = package_project(
            &PackageRequest {
                project_dir: dir.path().to_path_buf(),
                dockerfile: None,
                compose_file: None,
            },
            &output,
        )
        .unwrap();

        assert!(!package
            .included_files
            .iter()
            .any(|path| path == Path::new("bundle.tar.gz")));
    }
}
