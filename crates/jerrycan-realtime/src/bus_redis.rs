//! Redis pub/sub bus (feature `realtime-redis`): one channel carries the
//! serde-encoded `BusMessage` stream; every node PUBLISHes and every node's
//! pump forwards received messages into its local broadcast fan-in. Publishing
//! uses a lazily-connected ConnectionManager (register() is sync); subscribing
//! needs a dedicated pub/sub connection driven by `run_pump` in the supervisor.

use crate::bus::BusMessage;

pub(crate) const CHANNEL: &str = "jc:realtime:bus";
const FANIN_CAPACITY: usize = 1024;

pub(crate) struct RedisBus {
    url: String,
    publish_conn: tokio::sync::OnceCell<redis::aio::ConnectionManager>,
    fanin: tokio::sync::broadcast::Sender<BusMessage>,
}

fn redis_err(e: redis::RedisError) -> jerrycan_core::Error {
    eprintln!("jerrycan-realtime: redis: {e}");
    jerrycan_core::Error::internal("realtime redis bus error")
}

impl RedisBus {
    pub(crate) fn new(url: String) -> Self {
        Self {
            url,
            publish_conn: tokio::sync::OnceCell::new(),
            fanin: tokio::sync::broadcast::channel(FANIN_CAPACITY).0,
        }
    }

    async fn conn(&self) -> jerrycan_core::Result<redis::aio::ConnectionManager> {
        self.publish_conn
            .get_or_try_init(|| async {
                let client = redis::Client::open(self.url.as_str()).map_err(redis_err)?;
                redis::aio::ConnectionManager::new(client)
                    .await
                    .map_err(redis_err)
            })
            .await
            .map(Clone::clone)
    }

    pub(crate) async fn publish(&self, msg: BusMessage) -> jerrycan_core::Result<()> {
        let mut conn = self.conn().await?;
        let payload = serde_json::to_string(&msg).expect("bus messages serialize");
        let _: () = redis::cmd("PUBLISH")
            .arg(CHANNEL)
            .arg(payload)
            .query_async(&mut conn)
            .await
            .map_err(redis_err)?;
        Ok(())
    }

    pub(crate) fn subscribe(&self) -> tokio::sync::broadcast::Receiver<BusMessage> {
        self.fanin.subscribe()
    }

    /// The supervisor-run pump: subscribe to CHANNEL, decode each message, and
    /// forward it into the local fan-in. Reconnect with 1s→30s backoff;
    /// undecodable payloads are logged and skipped (version skew mid-deploy
    /// must not kill the pump).
    pub(crate) async fn run_pump(&self, mut shutdown: tokio::sync::watch::Receiver<bool>) {
        let mut backoff = std::time::Duration::from_secs(1);
        loop {
            if *shutdown.borrow() {
                return;
            }
            match self.pump_once(&mut shutdown).await {
                Ok(()) => return, // clean shutdown
                Err(e) => {
                    eprintln!("jerrycan-realtime: redis pump error: {e}");
                    tokio::select! {
                        _ = shutdown.changed() => return,
                        _ = tokio::time::sleep(backoff) => {}
                    }
                    backoff = (backoff * 2).min(std::time::Duration::from_secs(30));
                }
            }
        }
    }

    async fn pump_once(
        &self,
        shutdown: &mut tokio::sync::watch::Receiver<bool>,
    ) -> jerrycan_core::Result<()> {
        use futures_util::StreamExt;
        let client = redis::Client::open(self.url.as_str()).map_err(redis_err)?;
        let mut pubsub = client.get_async_pubsub().await.map_err(redis_err)?;
        pubsub.subscribe(CHANNEL).await.map_err(redis_err)?;
        let mut stream = pubsub.into_on_message();
        loop {
            let msg = tokio::select! {
                _ = shutdown.changed() => return Ok(()),
                m = stream.next() => match m {
                    Some(m) => m,
                    None => return Err(jerrycan_core::Error::internal("redis pubsub disconnected")),
                },
            };
            let Ok(payload) = msg.get_payload::<String>() else {
                continue;
            };
            match serde_json::from_str::<BusMessage>(&payload) {
                Ok(bm) => {
                    let _ = self.fanin.send(bm);
                }
                Err(e) => eprintln!("jerrycan-realtime: redis pump skipped undecodable message: {e}"),
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // -------------------------------------------------------------------
    // Live Redis bus test (multi-node fan-out). Needs a local Redis:
    //
    //   docker run --rm -d -p 6379:6379 redis:7
    //   cargo test -p jerrycan-realtime --features realtime-redis bus_redis \
    //     -- --ignored
    // -------------------------------------------------------------------
    #[tokio::test(flavor = "multi_thread")]
    #[ignore = "needs a local Redis (docker run --rm -d -p 6379:6379 redis:7)"]
    async fn two_buses_exchange_messages() {
        use std::sync::Arc;
        let url =
            std::env::var("JERRYCAN_REDIS_URL").unwrap_or_else(|_| "redis://127.0.0.1:6379".into());
        // Two "nodes", each with its own pump forwarding Redis → local fan-in.
        let a = Arc::new(RedisBus::new(url.clone()));
        let b = Arc::new(RedisBus::new(url));
        let (sa_tx, sa_rx) = tokio::sync::watch::channel(false);
        let (sb_tx, sb_rx) = tokio::sync::watch::channel(false);
        {
            let (a2, sd) = (a.clone(), sa_rx);
            tokio::spawn(async move { a2.run_pump(sd).await });
        }
        {
            let (b2, sd) = (b.clone(), sb_rx);
            tokio::spawn(async move { b2.run_pump(sd).await });
        }
        let mut ra = a.subscribe();
        let mut rb = b.subscribe();
        tokio::time::sleep(std::time::Duration::from_millis(300)).await;

        let msg = BusMessage::Resync {
            entity: Some("Lead".into()),
        };
        a.publish(msg.clone()).await.unwrap();

        // Both nodes (publisher's own included) receive the message.
        for rx in [&mut ra, &mut rb] {
            let got = tokio::time::timeout(std::time::Duration::from_secs(5), rx.recv())
                .await
                .expect("message within 5s")
                .expect("channel open");
            assert_eq!(got, msg);
        }
        let _ = sa_tx.send(true);
        let _ = sb_tx.send(true);
    }
}
