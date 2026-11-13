use postgres::types::ToSql;
use postgres::{Error, Row, ToStatement};
use std::time::Duration;
use tokio::runtime::{Builder, Runtime};
use tokio_postgres::{Client, Transaction};

/// A synchronous facade whose initial PostgreSQL handshake is bounded by a
/// cancelable Tokio timeout. The runtime remains attached to the client so it
/// can drive both the connection task and later synchronous storage calls.
pub(super) struct BoundedClient {
    runtime: Runtime,
    client: Client,
}

impl BoundedClient {
    pub(super) fn connect(dsn: &str, timeout: Duration) -> Result<Self, super::StorageError> {
        let mut config: tokio_postgres::Config = dsn.parse().map_err(super::pg_error)?;
        config.connect_timeout(timeout);

        let tls = native_tls::TlsConnector::builder()
            .build()
            .map_err(|error| {
                super::StorageError::Io(format!("PostgreSQL TLS 初始化失败: {error}"))
            })?;
        let connector = postgres_native_tls::MakeTlsConnector::new(tls);
        let runtime = Builder::new_current_thread()
            .enable_all()
            .build()
            .map_err(|error| {
                super::StorageError::Io(format!("PostgreSQL runtime 初始化失败: {error}"))
            })?;

        let (client, connection) = match runtime
            .block_on(async move { tokio::time::timeout(timeout, config.connect(connector)).await })
        {
            Ok(Ok(connection)) => connection,
            Ok(Err(error)) => {
                runtime.shutdown_timeout(Duration::ZERO);
                return Err(super::pg_error(error));
            }
            Err(_) => {
                // Tokio's system DNS lookup runs on spawn_blocking and cannot be
                // interrupted inside libc. Do not let Runtime::drop extend the
                // caller's connection deadline while waiting for that resolver.
                runtime.shutdown_timeout(Duration::ZERO);
                return Err(super::StorageError::Io(format!(
                    "PostgreSQL 建连总时限已到（{timeout:?}）"
                )));
            }
        };

        // The task is driven whenever a synchronous operation enters block_on.
        // Dropping the runtime closes the socket; dropping the timed-out connect
        // future above closes the not-yet-established socket immediately.
        runtime.spawn(async move {
            let _ = connection.await; // Client operations report connection termination.
        });

        Ok(Self { runtime, client })
    }

    pub(super) fn execute<T>(
        &mut self,
        query: &T,
        params: &[&(dyn ToSql + Sync)],
    ) -> Result<u64, Error>
    where
        T: ?Sized + ToStatement,
    {
        self.runtime.block_on(self.client.execute(query, params))
    }

    pub(super) fn query<T>(
        &mut self,
        query: &T,
        params: &[&(dyn ToSql + Sync)],
    ) -> Result<Vec<Row>, Error>
    where
        T: ?Sized + ToStatement,
    {
        self.runtime.block_on(self.client.query(query, params))
    }

    pub(super) fn query_one<T>(
        &mut self,
        query: &T,
        params: &[&(dyn ToSql + Sync)],
    ) -> Result<Row, Error>
    where
        T: ?Sized + ToStatement,
    {
        self.runtime.block_on(self.client.query_one(query, params))
    }

    pub(super) fn query_opt<T>(
        &mut self,
        query: &T,
        params: &[&(dyn ToSql + Sync)],
    ) -> Result<Option<Row>, Error>
    where
        T: ?Sized + ToStatement,
    {
        self.runtime.block_on(self.client.query_opt(query, params))
    }

    pub(super) fn batch_execute(&mut self, query: &str) -> Result<(), Error> {
        self.runtime.block_on(self.client.batch_execute(query))
    }

    pub(super) fn transaction(&mut self) -> Result<BoundedTransaction<'_>, Error> {
        let transaction = self.runtime.block_on(self.client.transaction())?;
        Ok(BoundedTransaction {
            runtime: &self.runtime,
            transaction: Some(transaction),
        })
    }
}

pub(super) struct BoundedTransaction<'a> {
    runtime: &'a Runtime,
    transaction: Option<Transaction<'a>>,
}

impl BoundedTransaction<'_> {
    // A bounded transaction is consumed by commit/rollback; the option is
    // absent only while its destructor is running, after all methods return.
    #[allow(clippy::expect_used)]
    fn transaction(&self) -> &Transaction<'_> {
        self.transaction
            .as_ref()
            .expect("transaction methods are unavailable after consumption")
    }

    pub(super) fn execute<T>(
        &mut self,
        query: &T,
        params: &[&(dyn ToSql + Sync)],
    ) -> Result<u64, Error>
    where
        T: ?Sized + ToStatement,
    {
        self.runtime
            .block_on(self.transaction().execute(query, params))
    }

    pub(super) fn query<T>(
        &mut self,
        query: &T,
        params: &[&(dyn ToSql + Sync)],
    ) -> Result<Vec<Row>, Error>
    where
        T: ?Sized + ToStatement,
    {
        self.runtime
            .block_on(self.transaction().query(query, params))
    }

    pub(super) fn query_one<T>(
        &mut self,
        query: &T,
        params: &[&(dyn ToSql + Sync)],
    ) -> Result<Row, Error>
    where
        T: ?Sized + ToStatement,
    {
        self.runtime
            .block_on(self.transaction().query_one(query, params))
    }

    pub(super) fn query_opt<T>(
        &mut self,
        query: &T,
        params: &[&(dyn ToSql + Sync)],
    ) -> Result<Option<Row>, Error>
    where
        T: ?Sized + ToStatement,
    {
        self.runtime
            .block_on(self.transaction().query_opt(query, params))
    }

    pub(super) fn batch_execute(&mut self, query: &str) -> Result<(), Error> {
        self.runtime
            .block_on(self.transaction().batch_execute(query))
    }

    #[allow(clippy::expect_used)]
    pub(super) fn commit(mut self) -> Result<(), Error> {
        let transaction = self
            .transaction
            .take()
            .expect("transaction is active until commit or rollback");
        self.runtime.block_on(transaction.commit())
    }

    #[allow(clippy::expect_used)]
    pub(super) fn rollback(mut self) -> Result<(), Error> {
        let transaction = self
            .transaction
            .take()
            .expect("transaction is active until commit or rollback");
        self.runtime.block_on(transaction.rollback())
    }
}

impl Drop for BoundedTransaction<'_> {
    fn drop(&mut self) {
        if let Some(transaction) = self.transaction.take() {
            // tokio-postgres queues a rollback when a Transaction is dropped,
            // but the current-thread runtime must be polled to send it. Drive
            // rollback before returning a pooled connection so locks from an
            // early error cannot block the next operation indefinitely.
            let _ = self.runtime.block_on(transaction.rollback());
        }
    }
}

pub(super) trait QueryClient {
    fn query<T>(&mut self, query: &T, params: &[&(dyn ToSql + Sync)]) -> Result<Vec<Row>, Error>
    where
        T: ?Sized + ToStatement;
}

impl QueryClient for BoundedClient {
    fn query<T>(&mut self, query: &T, params: &[&(dyn ToSql + Sync)]) -> Result<Vec<Row>, Error>
    where
        T: ?Sized + ToStatement,
    {
        BoundedClient::query(self, query, params)
    }
}

impl QueryClient for BoundedTransaction<'_> {
    fn query<T>(&mut self, query: &T, params: &[&(dyn ToSql + Sync)]) -> Result<Vec<Row>, Error>
    where
        T: ?Sized + ToStatement,
    {
        BoundedTransaction::query(self, query, params)
    }
}

#[cfg(test)]
mod tests {
    use super::BoundedClient;
    use std::io::{Read, Write};
    use std::net::TcpListener;
    use std::thread;
    use std::time::{Duration, Instant};

    #[test]
    fn connect_deadline_cancels_a_stalled_postgres_startup_handshake() {
        let listener = TcpListener::bind(("127.0.0.1", 0)).expect("bind local PostgreSQL stub");
        let address = listener.local_addr().expect("read local stub address");
        let server = thread::spawn(move || {
            let (mut stream, _) = listener.accept().expect("accept client");
            stream
                .set_read_timeout(Some(Duration::from_secs(2)))
                .expect("set stub read timeout");

            let mut length = [0_u8; 4];
            stream.read_exact(&mut length).expect("read startup length");
            let length = u32::from_be_bytes(length) as usize;
            assert!((8..=16 * 1024).contains(&length));
            let mut startup = vec![0_u8; length - 4];
            stream
                .read_exact(&mut startup)
                .expect("read complete startup packet");

            let mut probe = [0_u8; 1];
            loop {
                match stream.read(&mut probe) {
                    Ok(0) => break true,
                    Ok(_) => continue,
                    Err(_) => break false,
                }
            }
        });

        let deadline = Duration::from_millis(150);
        let started = Instant::now();
        let error = match BoundedClient::connect(
            &format!(
                "host=127.0.0.1 port={} user=qx dbname=qx sslmode=disable",
                address.port()
            ),
            deadline,
        ) {
            Ok(_) => panic!("the startup handshake must not wait for a PostgreSQL response"),
            Err(error) => error,
        };
        let elapsed = started.elapsed();

        assert!(
            matches!(
                error,
                super::super::StorageError::Io(ref message)
                    if message.contains("建连总时限已到")
            ),
            "unexpected error: {error:?}"
        );
        assert!(
            elapsed < Duration::from_secs(2),
            "deadline took {elapsed:?}"
        );
        assert!(
            server.join().expect("join PostgreSQL stub"),
            "timed-out connect must close its socket"
        );
    }

    #[test]
    fn connect_deadline_cancels_a_stalled_tls_handshake() {
        let listener = TcpListener::bind(("127.0.0.1", 0)).expect("bind local PostgreSQL stub");
        let address = listener.local_addr().expect("read local stub address");
        let server = thread::spawn(move || {
            let (mut stream, _) = listener.accept().expect("accept client");
            stream
                .set_read_timeout(Some(Duration::from_secs(2)))
                .expect("set stub read timeout");

            let mut request = [0_u8; 8];
            stream.read_exact(&mut request).expect("read SSLRequest");
            assert_eq!(u32::from_be_bytes(request[..4].try_into().unwrap()), 8);
            assert_eq!(
                u32::from_be_bytes(request[4..].try_into().unwrap()),
                80_877_103
            );
            stream.write_all(b"S").expect("accept TLS negotiation");

            let mut probe = [0_u8; 1];
            loop {
                match stream.read(&mut probe) {
                    Ok(0) => break true,
                    Ok(_) => continue,
                    Err(_) => break false,
                }
            }
        });

        let deadline = Duration::from_millis(150);
        let started = Instant::now();
        let error = match BoundedClient::connect(
            &format!(
                "host=127.0.0.1 port={} user=qx dbname=qx sslmode=require",
                address.port()
            ),
            deadline,
        ) {
            Ok(_) => panic!("the TLS handshake must not wait for a PostgreSQL response"),
            Err(error) => error,
        };
        let elapsed = started.elapsed();

        assert!(
            matches!(
                error,
                super::super::StorageError::Io(ref message)
                    if message.contains("建连总时限已到")
            ),
            "unexpected error: {error:?}"
        );
        assert!(
            elapsed < Duration::from_secs(2),
            "deadline took {elapsed:?}"
        );
        assert!(
            server.join().expect("join PostgreSQL stub"),
            "timed-out TLS connect must close its socket"
        );
    }
}
