use std::path::PathBuf;
use std::time::{SystemTime, UNIX_EPOCH};

use tempfile::tempdir;
use thiserror::Error;

use subdeploy_packager::{package_project, PackageError, PackageRequest};
use subdeploy_remote::{RemoteError, RemoteTransport};

use crate::health::{wait_for_health, HealthCheckError, HealthCheckSpec};

#[derive(Debug, Clone)]
pub struct ComposeSpec {
    pub dockerfile_rel: PathBuf,
    pub compose_file_rel: PathBuf,
    pub service_name: String,
    pub image_tag: String,
}

#[derive(Debug, Clone)]
pub struct DeployRequest {
    pub package_request: PackageRequest,
    pub remote_dir: String,
    pub ssh_host: String,
    pub ssh_port: u16,
    pub ssh_user: String,
    pub ssh_password: String,
    pub compose: ComposeSpec,
    pub no_cache: bool,
    pub health_check: Option<HealthCheckSpec>,
}

#[derive(Debug, Clone)]
pub struct DeployResult {
    pub remote_archive_path: String,
    pub release_id: String,
}

#[derive(Debug, Error)]
pub enum DeployError {
    #[error(transparent)]
    Package(#[from] PackageError),
    #[error(transparent)]
    Remote(#[from] RemoteError),
    #[error(transparent)]
    Health(#[from] HealthCheckError),
    #[error("生成临时归档失败: {0}")]
    Tempfile(#[from] std::io::Error),
}

pub fn default_remote_dir(project_name: &str) -> String {
    format!("/root/{project_name}-deploy")
}

pub fn deploy(
    request: &DeployRequest,
    remote: &mut dyn RemoteTransport,
) -> Result<DeployResult, DeployError> {
    let temp_dir = tempdir()?;
    let archive_path = temp_dir.path().join("deployment.tar.gz");
    let package = package_project(&request.package_request, &archive_path)?;
    let release_id = build_release_id();
    let remote_archive_path = format!(
        "/tmp/{}-{}.tar.gz",
        package.project_name,
        release_id.replace('/', "-")
    );

    remote.upload_file(&archive_path, &remote_archive_path)?;
    let script = render_remote_script(request, &remote_archive_path, &release_id);
    remote.run_script(&script)?;

    if let Some(spec) = &request.health_check {
        wait_for_health(spec)?;
    }

    Ok(DeployResult {
        remote_archive_path,
        release_id,
    })
}

fn build_release_id() -> String {
    let timestamp = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs();
    format!("{timestamp}-{}", std::process::id())
}

fn render_remote_script(
    request: &DeployRequest,
    remote_archive_path: &str,
    release_id: &str,
) -> String {
    let releases_dir = format!("{}/releases", request.remote_dir);
    let release_dir = format!("{releases_dir}/{release_id}");
    let current_link = format!("{}/current", request.remote_dir);
    let no_cache = if request.no_cache { "--no-cache " } else { "" };

    format!(
        r#"set -euo pipefail
REMOTE_DIR={remote_dir}
RELEASES_DIR={releases_dir}
RELEASE_DIR={release_dir}
CURRENT_LINK={current_link}
REMOTE_ARCHIVE={remote_archive}
mkdir -p "$RELEASES_DIR"

if [ -L "$CURRENT_LINK" ] || [ -d "$CURRENT_LINK" ]; then
  OLD_CURRENT="$(readlink -f "$CURRENT_LINK" || true)"
  if [ -n "${{OLD_CURRENT:-}}" ] && [ -f "$OLD_CURRENT/{compose_file}" ]; then
    cd "$OLD_CURRENT"
    docker compose -f {compose_file} down || true
  fi
fi

rm -rf "$RELEASE_DIR"
mkdir -p "$RELEASE_DIR"
tar -xzf "$REMOTE_ARCHIVE" -C "$RELEASE_DIR"
rm -f "$REMOTE_ARCHIVE"

ln -sfn "$RELEASE_DIR" "$CURRENT_LINK"
cd "$CURRENT_LINK"

docker build {no_cache}-t {image_tag} -f {dockerfile} .
docker compose -f {compose_file} up -d
docker compose -f {compose_file} ps
docker logs {service_name} --tail 50 || true
"#,
        remote_dir = shell_quote(&request.remote_dir),
        releases_dir = shell_quote(&releases_dir),
        release_dir = shell_quote(&release_dir),
        current_link = shell_quote(&current_link),
        remote_archive = shell_quote(remote_archive_path),
        dockerfile = shell_quote_path(&request.compose.dockerfile_rel),
        compose_file = shell_quote_path(&request.compose.compose_file_rel),
        image_tag = shell_quote(&request.compose.image_tag),
        service_name = shell_quote(&request.compose.service_name),
        no_cache = no_cache,
    )
}

fn shell_quote(value: &str) -> String {
    format!("'{}'", value.replace('\'', r"'\''"))
}

fn shell_quote_path(path: &std::path::Path) -> String {
    shell_quote(&path.to_string_lossy())
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::path::Path;
    use std::sync::{Arc, Mutex};
    use std::time::Duration;

    use tempfile::tempdir;

    use super::{
        default_remote_dir, deploy, render_remote_script, ComposeSpec, DeployRequest,
        HealthCheckSpec,
    };
    use subdeploy_packager::PackageRequest;
    use subdeploy_remote::{RemoteError, RemoteTransport};

    #[derive(Clone, Default)]
    struct FakeRemote {
        uploads: Arc<Mutex<Vec<String>>>,
        scripts: Arc<Mutex<Vec<String>>>,
    }

    impl RemoteTransport for FakeRemote {
        fn upload_file(
            &mut self,
            _local_path: &Path,
            remote_path: &str,
        ) -> Result<(), RemoteError> {
            self.uploads.lock().unwrap().push(remote_path.to_owned());
            Ok(())
        }

        fn run_script(&mut self, script: &str) -> Result<(), RemoteError> {
            self.scripts.lock().unwrap().push(script.to_owned());
            Ok(())
        }
    }

    #[test]
    fn default_remote_dir_uses_project_name() {
        assert_eq!(default_remote_dir("demo"), "/root/demo-deploy");
    }

    #[test]
    fn rendered_script_keeps_expected_order() {
        let request = sample_request();
        let script = render_remote_script(&request, "/tmp/demo.tar.gz", "1-2");

        let down_idx = script.find("docker compose -f").unwrap();
        let build_idx = script.find("docker build").unwrap();
        let up_idx = script.rfind("docker compose -f").unwrap();

        assert!(down_idx < build_idx);
        assert!(build_idx < up_idx);
        assert!(script.contains("ln -sfn"));
    }

    #[test]
    fn deploy_uploads_archive_and_executes_script() {
        let dir = tempdir().unwrap();
        fs::write(dir.path().join("Dockerfile"), "FROM scratch\n").unwrap();
        fs::write(dir.path().join("docker-compose.yml"), "services: {}\n").unwrap();
        fs::write(dir.path().join("main.rs"), "fn main() {}\n").unwrap();

        let request = DeployRequest {
            package_request: PackageRequest {
                project_dir: dir.path().to_path_buf(),
                dockerfile: None,
                compose_file: None,
            },
            ..sample_request()
        };
        let request = DeployRequest {
            health_check: None,
            ..request
        };
        let mut remote = FakeRemote::default();
        let result = deploy(&request, &mut remote).unwrap();

        assert!(result.remote_archive_path.starts_with("/tmp/"));
        assert_eq!(remote.uploads.lock().unwrap().len(), 1);
        assert_eq!(remote.scripts.lock().unwrap().len(), 1);
    }

    #[test]
    fn compose_detection_prefers_docker_compose_yml() {
        let dir = tempdir().unwrap();
        fs::write(dir.path().join("Dockerfile"), "FROM scratch\n").unwrap();
        fs::write(dir.path().join("docker-compose.yml"), "services: {}\n").unwrap();
        fs::write(dir.path().join("compose.yml"), "services: {alt: {}}\n").unwrap();

        let request = DeployRequest {
            package_request: PackageRequest {
                project_dir: dir.path().to_path_buf(),
                dockerfile: None,
                compose_file: None,
            },
            ..sample_request()
        };

        let script = render_remote_script(&request, "/tmp/demo.tar.gz", "x");
        assert!(script.contains("docker-compose.yml"));
    }

    fn sample_request() -> DeployRequest {
        DeployRequest {
            package_request: PackageRequest {
                project_dir: Path::new(".").to_path_buf(),
                dockerfile: None,
                compose_file: None,
            },
            remote_dir: "/root/demo-deploy".to_owned(),
            ssh_host: "example.com".to_owned(),
            ssh_port: 22,
            ssh_user: "root".to_owned(),
            ssh_password: "secret".to_owned(),
            compose: ComposeSpec {
                dockerfile_rel: Path::new("Dockerfile").to_path_buf(),
                compose_file_rel: Path::new("docker-compose.yml").to_path_buf(),
                service_name: "demo".to_owned(),
                image_tag: "demo:latest".to_owned(),
            },
            no_cache: true,
            health_check: Some(HealthCheckSpec {
                url: "http://127.0.0.1/health".to_owned(),
                timeout: Duration::from_secs(1),
                poll_interval: Duration::from_millis(50),
            }),
        }
    }
}
