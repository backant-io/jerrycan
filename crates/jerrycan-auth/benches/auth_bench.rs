//! Crypto hot paths: session encode/decode and JWT encode/decode.
use criterion::{Criterion, criterion_group, criterion_main};
use jerrycan_auth::{SessionStore, jwt};
use serde::{Deserialize, Serialize};

#[derive(Serialize, Deserialize)]
struct Sess {
    id: i64,
    role: String,
}

fn bench_session(c: &mut Criterion) {
    let store = SessionStore::new(&[9u8; 32]);
    let token = store
        .encode(&Sess {
            id: 1,
            role: "admin".into(),
        })
        .unwrap();
    c.bench_function("session_encode", |b| {
        b.iter(|| {
            store
                .encode(&Sess {
                    id: 1,
                    role: "admin".into(),
                })
                .unwrap()
        });
    });
    c.bench_function("session_decode", |b| {
        b.iter(|| store.decode::<Sess>(&token).unwrap());
    });
}

fn bench_jwt(c: &mut Criterion) {
    let key = [9u8; 32];
    let token = jwt::encode(
        &Sess {
            id: 1,
            role: "admin".into(),
        },
        &key,
    )
    .unwrap();
    c.bench_function("jwt_encode", |b| {
        b.iter(|| {
            jwt::encode(
                &Sess {
                    id: 1,
                    role: "admin".into(),
                },
                &key,
            )
            .unwrap()
        })
    });
    c.bench_function("jwt_decode", |b| {
        b.iter(|| jwt::decode::<Sess>(&token, &key).unwrap())
    });
}

criterion_group!(benches, bench_session, bench_jwt);
criterion_main!(benches);
