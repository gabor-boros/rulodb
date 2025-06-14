use byteorder::{BigEndian, WriteBytesExt};
use log::{debug, error, info};
use rmpv::Value;
use rulodb::Datum;
use rulodb::Evaluator;
use rulodb::Parser;
use rulodb::Planner;
use rulodb::StorageBackend;
use rust_decimal::prelude::ToPrimitive;
use std::sync::Arc;
use tokio::io::{AsyncReadExt, AsyncWriteExt, BufReader};
use tokio::net::{TcpListener, TcpStream};
pub async fn start_server(
    db: Arc<dyn StorageBackend + Send + Sync>,
    address: &str,
) -> anyhow::Result<()> {
    let listener = TcpListener::bind(address).await?;
    info!("server listening on {address}");

    loop {
        let (stream, _) = listener.accept().await?;
        let db = db.clone();
        tokio::spawn(async move {
            if let Err(e) = handle_client(db, stream).await {
                log::error!("client error: {e}");
            }
        });
    }
}

async fn handle_client(
    db: Arc<dyn StorageBackend + Send + Sync>,
    stream: TcpStream,
) -> anyhow::Result<()> {
    let peer = stream.peer_addr()?;
    let (read_half, mut write_half) = stream.into_split();
    let mut reader = BufReader::new(read_half);

    loop {
        let mut len_buf = [0u8; 4];
        if reader.read_exact(&mut len_buf).await.is_err() {
            break;
        }
        let msg_len = u32::from_be_bytes(len_buf) as usize;

        let mut buffer = vec![0u8; msg_len];
        if reader.read_exact(&mut buffer).await.is_err() {
            break;
        }

        let response = process_msgpack_line(db.clone(), &buffer)
            .await
            .unwrap_or_else(|err| {
                error!("failed to process line from {peer}: {err}");
                Value::Map(vec![(Value::from("error"), Value::from(err.to_string()))])
            });

        let mut payload = Vec::new();
        rmpv::encode::write_value(&mut payload, &response)?;

        let mut out: Vec<u8> = Vec::new();
        WriteBytesExt::write_u32::<BigEndian>(&mut out, u32::try_from(payload.len())?)?;
        out.extend(payload);

        write_half.write_all(&out).await?;
    }

    Ok(())
}

async fn process_msgpack_line(
    db: Arc<dyn StorageBackend + Send + Sync>,
    line: &[u8],
) -> anyhow::Result<Value> {
    let value: Value = rmpv::decode::read_value(&mut &*line)?;
    let term = Parser::new().parse(&value)?;

    let mut planner = Planner::new();
    let plan = planner.plan(&term)?;
    let plan = planner.optimize(plan);
    let explanation = planner.explain(&plan, 0);

    debug!("{explanation}");

    let mut evaluator = Evaluator::new(db);
    let result = evaluator.eval(&plan).await?;

    Ok(datum_to_rmpv(result.result))
}

fn datum_to_rmpv(datum: Datum) -> Value {
    match datum {
        Datum::String(s) | Datum::Parameter(s) => Value::String(s.into()),
        Datum::Integer(i) => Value::Integer(i.into()),
        Datum::Decimal(d) => Value::F64(d.to_f64().unwrap()),
        Datum::Bool(b) => Value::Boolean(b),
        Datum::Null => Value::Nil,
        Datum::Array(arr) => Value::Array(arr.into_iter().map(datum_to_rmpv).collect()),
        Datum::Object(obj) => {
            let map = obj
                .into_iter()
                .map(|(k, v)| (Value::String(k.into()), datum_to_rmpv(v)))
                .collect();

            Value::Map(map)
        }
    }
}
