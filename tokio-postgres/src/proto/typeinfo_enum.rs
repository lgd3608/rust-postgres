use futures::stream::{self, Stream};
use futures::{Async, Future, Poll};
use state_machine_future::RentToOwn;

use bad_response;
use error::{Error, SqlState};
use proto::client::Client;
use proto::prepare::PrepareFuture;
use proto::query::QueryStream;
use types::Oid;

const TYPEINFO_ENUM_NAME: &'static str = "_rust_typeinfo_enum";

const TYPEINFO_ENUM_QUERY: &'static str = "
SELECT enumlabel
FROM pg_catalog.pg_enum
WHERE enumtypid = $1
ORDER BY enumsortorder
";

// Postgres 9.0 didn't have enumsortorder
const TYPEINFO_ENUM_FALLBACK_QUERY: &'static str = "
SELECT enumlabel
FROM pg_catalog.pg_enum
WHERE enumtypid = $1
ORDER BY oid
";

#[derive(StateMachineFuture)]
pub enum TypeinfoEnum {
    #[state_machine_future(start, transitions(PreparingTypeinfoEnum, QueryingEnumVariants))]
    Start { oid: Oid, client: Client },
    #[state_machine_future(transitions(PreparingTypeinfoEnumFallback, QueryingEnumVariants))]
    PreparingTypeinfoEnum {
        future: Box<PrepareFuture>,
        oid: Oid,
        client: Client,
    },
    #[state_machine_future(transitions(QueryingEnumVariants))]
    PreparingTypeinfoEnumFallback {
        future: Box<PrepareFuture>,
        oid: Oid,
        client: Client,
    },
    #[state_machine_future(transitions(Finished))]
    QueryingEnumVariants {
        future: stream::Collect<QueryStream>,
        client: Client,
    },
    #[state_machine_future(ready)]
    Finished((Vec<String>, Client)),
    #[state_machine_future(error)]
    Failed(Error),
}

impl PollTypeinfoEnum for TypeinfoEnum {
    fn poll_start<'a>(state: &'a mut RentToOwn<'a, Start>) -> Poll<AfterStart, Error> {
        let mut state = state.take();

        let statement = state.client.state.lock().typeinfo_enum_query.clone();
        match statement {
            Some(statement) => transition!(QueryingEnumVariants {
                future: state.client.query(&statement, &[&state.oid]).collect(),
                client: state.client,
            }),
            None => transition!(PreparingTypeinfoEnum {
                future: Box::new(state.client.prepare(
                    TYPEINFO_ENUM_NAME.to_string(),
                    TYPEINFO_ENUM_QUERY,
                    &[]
                )),
                oid: state.oid,
                client: state.client,
            }),
        }
    }

    fn poll_preparing_typeinfo_enum<'a>(
        state: &'a mut RentToOwn<'a, PreparingTypeinfoEnum>,
    ) -> Poll<AfterPreparingTypeinfoEnum, Error> {
        let statement = match state.future.poll() {
            Ok(Async::Ready(statement)) => statement,
            Ok(Async::NotReady) => return Ok(Async::NotReady),
            Err(ref e) if e.code() == Some(&SqlState::UNDEFINED_COLUMN) => {
                let mut state = state.take();

                transition!(PreparingTypeinfoEnumFallback {
                    future: Box::new(state.client.prepare(
                        TYPEINFO_ENUM_NAME.to_string(),
                        TYPEINFO_ENUM_FALLBACK_QUERY,
                        &[]
                    )),
                    oid: state.oid,
                    client: state.client,
                })
            }
            Err(e) => return Err(e),
        };
        let mut state = state.take();

        state.client.state.lock().typeinfo_enum_query = Some(statement.clone());
        transition!(QueryingEnumVariants {
            future: state.client.query(&statement, &[&state.oid]).collect(),
            client: state.client,
        })
    }

    fn poll_preparing_typeinfo_enum_fallback<'a>(
        state: &'a mut RentToOwn<'a, PreparingTypeinfoEnumFallback>,
    ) -> Poll<AfterPreparingTypeinfoEnumFallback, Error> {
        let statement = try_ready!(state.future.poll());
        let mut state = state.take();

        state.client.state.lock().typeinfo_enum_query = Some(statement.clone());
        transition!(QueryingEnumVariants {
            future: state.client.query(&statement, &[&state.oid]).collect(),
            client: state.client,
        })
    }

    fn poll_querying_enum_variants<'a>(
        state: &'a mut RentToOwn<'a, QueryingEnumVariants>,
    ) -> Poll<AfterQueryingEnumVariants, Error> {
        let rows = try_ready!(state.future.poll());
        let state = state.take();

        let variants = rows
            .iter()
            .map(|row| row.try_get(0)?.ok_or_else(bad_response))
            .collect::<Result<Vec<_>, _>>()?;

        transition!(Finished((variants, state.client)))
    }
}

impl TypeinfoEnumFuture {
    pub fn new(oid: Oid, client: Client) -> TypeinfoEnumFuture {
        TypeinfoEnum::start(oid, client)
    }
}
