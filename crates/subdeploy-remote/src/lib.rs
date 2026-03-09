use std::fs::File;
use std::io::{self, Read, Write};
use std::net::{TcpStream, ToSocketAddrs};
use std::path::Path;
use std::time::Duration;

use ssh2::Session;
use thiserror::Error;

#[derive(Debug, Error)]
pub enum RemoteError {
    #[error("无法解析 SSH 地址: {0}")]
    Resolve(String),
    #[error("建立 TCP 连接失败: {0}")]
    TcpConnect(#[source] io::Error),
    #[error("创建 SSH 会话失败")]
    CreateSession,
    #[error("SSH 握手失败: {0}")]
    Handshake(#[source] ssh2::Error),
    #[error("SSH 密码认证失败: {0}")]
    Auth(#[source] ssh2::Error),
    #[error("SFTP 上传失败: {0}")]
    Upload(#[source] io::Error),
    #[error("远端命令执行失败: {0}")]
    Exec(#[source] io::Error),
    #[error("远端命令退出码非零: {0}")]
    ExitStatus(i32),
    #[error("SSH 协议错误: {0}")]
    Ssh(#[from] ssh2::Error),
}

pub trait RemoteTransport {
    fn upload_file(&mut self, local_path: &Path, remote_path: &str) -> Result<(), RemoteError>;
    fn run_script(&mut self, script: &str) -> Result<(), RemoteError>;
}

pub struct SshRemote {
    session: Session,
}

impl SshRemote {
    pub fn connect(host: &str, port: u16, user: &str, password: &str) -> Result<Self, RemoteError> {
        let address = format!("{host}:{port}");
        let socket_addr = address
            .to_socket_addrs()
            .map_err(|_| RemoteError::Resolve(address.clone()))?
            .next()
            .ok_or_else(|| RemoteError::Resolve(address.clone()))?;

        let stream = TcpStream::connect_timeout(&socket_addr, Duration::from_secs(20))
            .map_err(RemoteError::TcpConnect)?;
        stream
            .set_read_timeout(Some(Duration::from_secs(20)))
            .map_err(RemoteError::TcpConnect)?;
        stream
            .set_write_timeout(Some(Duration::from_secs(20)))
            .map_err(RemoteError::TcpConnect)?;

        let mut session = Session::new().map_err(|_| RemoteError::CreateSession)?;
        session.set_tcp_stream(stream);
        session.set_timeout(20_000);
        session.handshake().map_err(RemoteError::Handshake)?;
        session
            .userauth_password(user, password)
            .map_err(RemoteError::Auth)?;

        Ok(Self { session })
    }
}

impl RemoteTransport for SshRemote {
    fn upload_file(&mut self, local_path: &Path, remote_path: &str) -> Result<(), RemoteError> {
        let mut local_file = File::open(local_path).map_err(RemoteError::Upload)?;
        let sftp = self.session.sftp()?;
        let mut remote_file = sftp.create(Path::new(remote_path))?;

        io::copy(&mut local_file, &mut remote_file).map_err(RemoteError::Upload)?;
        remote_file.flush().map_err(RemoteError::Upload)?;
        Ok(())
    }

    fn run_script(&mut self, script: &str) -> Result<(), RemoteError> {
        let mut channel = self.session.channel_session()?;
        channel.request_pty("xterm", None, Some((120, 30, 0, 0)))?;
        // 合并 stderr 到 stdout，避免双流读取时阻塞。
        channel.exec("bash -s 2>&1")?;
        channel
            .write_all(script.as_bytes())
            .map_err(RemoteError::Exec)?;
        channel.send_eof()?;

        let mut stdout = io::stdout();
        let mut buffer = [0_u8; 4096];
        loop {
            let read = channel.read(&mut buffer).map_err(RemoteError::Exec)?;
            if read == 0 {
                break;
            }
            stdout
                .write_all(&buffer[..read])
                .map_err(RemoteError::Exec)?;
            stdout.flush().map_err(RemoteError::Exec)?;
        }

        channel.wait_close()?;
        let exit_status = channel.exit_status()?;
        if exit_status != 0 {
            return Err(RemoteError::ExitStatus(exit_status));
        }

        Ok(())
    }
}
