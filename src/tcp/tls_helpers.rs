use byteorder::{ByteOrder, NetworkEndian, ReadBytesExt};
use rcgen::generate_simple_self_signed;
use smol::{io::BufReader, net::TcpStream, prelude::*};

/// Negotiates an *optional* TLS connection.
pub async fn opportunistic_tls_serve(client: TcpStream) -> anyhow::Result<CompositeReadWrite> {
    let mut client_up = BufReader::with_capacity(65536, client.clone());
    let initial = client_up.fill_buf().await?;
    // Checks to see whether the initial bit looks like a clienthello at all
    let is_tls = guess_client_hello(initial).is_ok();
    if !is_tls {
        tracing::debug!("not tls");
        Ok(CompositeReadWrite {
            reader: Box::new(client_up),
            writer: Box::new(client.clone()),
        })
    } else {
        tracing::debug!("yes tls");
        let composite = CompositeReadWrite {
            reader: Box::new(client_up),
            writer: Box::new(client),
        };
        let names = vec![format!(
            "{}{}.com",
            eff_wordlist::large::random_word(),
            eff_wordlist::large::random_word()
        )];
        let cert = generate_simple_self_signed(names)?;
        let cert_pem = cert.serialize_pem()?;
        let cert_key = cert.serialize_private_key_pem();
        let identity = native_tls::Identity::from_pkcs8(cert_pem.as_bytes(), cert_key.as_bytes())
            .expect("wtf cannot decode id???");
        let acceptor: async_native_tls::TlsAcceptor =
            native_tls::TlsAcceptor::new(identity)?.into();
        let client = acceptor.accept(composite).await?;
        let client = async_dup::Arc::new(async_dup::Mutex::new(client));
        Ok(CompositeReadWrite {
            reader: Box::new(client.clone()),
            writer: Box::new(client.clone()),
        })
    }
}

pub struct CompositeReadWrite {
    reader: Box<dyn AsyncRead + Send + 'static>,
    writer: Box<dyn AsyncWrite + Send + 'static>,
}

impl AsyncRead for CompositeReadWrite {
    fn poll_read(
        self: std::pin::Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
        buf: &mut [u8],
    ) -> std::task::Poll<std::io::Result<usize>> {
        let pinned = unsafe { self.map_unchecked_mut(|s| s.reader.as_mut()) };
        pinned.poll_read(cx, buf)
    }
}

impl AsyncWrite for CompositeReadWrite {
    fn poll_write(
        self: std::pin::Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
        buf: &[u8],
    ) -> std::task::Poll<std::io::Result<usize>> {
        let pinned = unsafe { self.map_unchecked_mut(|s| s.writer.as_mut()) };
        pinned.poll_write(cx, buf)
    }

    fn poll_close(
        self: std::pin::Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<std::io::Result<()>> {
        let pinned = unsafe { self.map_unchecked_mut(|s| s.writer.as_mut()) };
        pinned.poll_close(cx)
    }

    fn poll_flush(
        self: std::pin::Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<std::io::Result<()>> {
        let pinned = unsafe { self.map_unchecked_mut(|s| s.writer.as_mut()) };
        pinned.poll_flush(cx)
    }
}

fn guess_client_hello<R: std::io::Read>(mut reader: R) -> std::io::Result<()> {
    // skip the first 5 bytes (the header for the container containing the clienthello)
    for _ in 0..5 {
        reader.read_u8()?;
    }
    // Handshake message type.
    const HANDSHAKE_TYPE_CLIENT_HELLO: u8 = 1;
    let typ = reader.read_u8()?;
    if typ != HANDSHAKE_TYPE_CLIENT_HELLO {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            format!(
                "handshake message not a ClientHello (type {}, expected {})",
                typ, HANDSHAKE_TYPE_CLIENT_HELLO
            ),
        ));
    }
    // Handshake message length.
    let len = read_u24(&mut reader)?;
    if len < 10000 {
        Ok(())
    } else {
        Err(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            "bad length lol",
        ))
    }
}

fn read_u24<R: std::io::Read>(mut reader: R) -> std::io::Result<u32> {
    let mut buf = [0; 3];
    reader
        .read_exact(&mut buf)
        .map(|_| NetworkEndian::read_u24(&buf))
}
