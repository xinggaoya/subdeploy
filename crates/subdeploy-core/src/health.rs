use std::thread;
use std::time::{Duration, Instant};

use reqwest::blocking::Client;
use thiserror::Error;

#[derive(Debug, Clone)]
pub struct HealthCheckSpec {
    pub url: String,
    pub timeout: Duration,
    pub poll_interval: Duration,
}

#[derive(Debug, Error)]
pub enum HealthCheckError {
    #[error("健康检查超时，最后一次错误: {last_error}")]
    Timeout { last_error: String },
    #[error("创建 HTTP 客户端失败: {0}")]
    Client(#[from] reqwest::Error),
}

pub fn wait_for_health(spec: &HealthCheckSpec) -> Result<(), HealthCheckError> {
    let client = Client::builder()
        .timeout(Duration::from_secs(10))
        .redirect(reqwest::redirect::Policy::none())
        .build()?;
    let deadline = Instant::now() + spec.timeout;
    let mut last_error = String::from("未发起请求");

    while Instant::now() < deadline {
        match client.get(&spec.url).send() {
            Ok(response) => {
                let status = response.status();
                if status.is_success() || status.is_redirection() {
                    return Ok(());
                }
                last_error = format!("HTTP {}", status.as_u16());
            }
            Err(error) => {
                last_error = error.to_string();
            }
        }

        thread::sleep(spec.poll_interval);
    }

    Err(HealthCheckError::Timeout { last_error })
}

#[cfg(test)]
mod tests {
    use std::io::{Read, Write};
    use std::net::TcpListener;
    use std::thread;
    use std::time::Duration;

    use super::{wait_for_health, HealthCheckError, HealthCheckSpec};

    #[test]
    fn wait_for_health_accepts_redirect_response() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();

        thread::spawn(move || {
            if let Ok((mut stream, _)) = listener.accept() {
                let mut request = [0_u8; 1024];
                let _ = stream.read(&mut request);
                let response =
                    b"HTTP/1.1 302 Found\r\nLocation: /ready\r\nContent-Length: 0\r\n\r\n";
                stream.write_all(response).unwrap();
            }
        });

        let spec = HealthCheckSpec {
            url: format!("http://{address}"),
            timeout: Duration::from_secs(2),
            poll_interval: Duration::from_millis(50),
        };

        wait_for_health(&spec).unwrap();
    }

    #[test]
    fn wait_for_health_times_out_on_connection_refused() {
        let spec = HealthCheckSpec {
            url: "http://127.0.0.1:9".to_owned(),
            timeout: Duration::from_millis(250),
            poll_interval: Duration::from_millis(50),
        };

        let error = wait_for_health(&spec).unwrap_err();
        assert!(matches!(error, HealthCheckError::Timeout { .. }));
    }
}
