//! Refusal and partial-success paths of the IPC client against a mock KiCad.
//!
//! Every test pins an observable guarantee: a reply that is malformed, short,
//! or only partly successful is reported as a failure that names what KiCad
//! did, and an empty request never reaches the wire.

use kiapi::common::commands::{ItemStatus, ItemStatusCode};
use kiapi::common::types::ItemRequestStatus;
use konnect_ipc::builders;
use konnect_ipc::client::IpcOutcomeUnknown;
use konnect_ipc::gen::kiapi;
use konnect_ipc::{ApiStatusError, KiCadIpcClient, PingOutcome};
use nng::options::Options;
use prost::Message;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

struct Mock {
    url: String,
    requests: Arc<AtomicUsize>,
    _thread: std::thread::JoinHandle<()>,
}

/// What the mock KiCad answers: encoded bytes, or nothing (a wedged KiCad).
type Reply = Option<Vec<u8>>;

fn spawn<F>(respond: F) -> Mock
where
    F: Fn(&str) -> Reply + Send + 'static,
{
    static NEXT: AtomicUsize = AtomicUsize::new(0);
    let url = format!(
        "inproc://wire-item-mock-{}",
        NEXT.fetch_add(1, Ordering::Relaxed)
    );
    let socket = nng::Socket::new(nng::Protocol::Rep0).expect("mock socket");
    socket
        .set_opt::<nng::options::RecvTimeout>(Some(Duration::from_secs(20)))
        .unwrap();
    socket.listen(&url).expect("mock listen");
    let requests = Arc::new(AtomicUsize::new(0));
    let counted = requests.clone();
    let thread = std::thread::spawn(move || {
        while let Ok(msg) = socket.recv() {
            let Ok(request) = kiapi::common::ApiRequest::decode(msg.as_slice()) else {
                break;
            };
            counted.fetch_add(1, Ordering::SeqCst);
            let type_url = request.message.expect("request message").type_url;
            let short = type_url.rsplit('.').next().unwrap_or("").to_string();
            match respond(&short) {
                Some(bytes) => {
                    if socket.send(nng::Message::from(bytes.as_slice())).is_err() {
                        break;
                    }
                }
                None => {
                    std::thread::sleep(Duration::from_secs(20));
                    break;
                }
            }
        }
    });
    Mock {
        url,
        requests,
        _thread: thread,
    }
}

fn status(code: kiapi::common::ApiStatusCode, message: &str) -> kiapi::common::ApiResponseStatus {
    kiapi::common::ApiResponseStatus {
        status: code as i32,
        error_message: message.to_string(),
    }
}

fn envelope(inner: Option<prost_types::Any>) -> Reply {
    Some(
        kiapi::common::ApiResponse {
            status: Some(status(kiapi::common::ApiStatusCode::AsOk, "")),
            header: None,
            message: inner,
        }
        .encode_to_vec(),
    )
}

fn reply<M: Message>(message: &M, name: &str) -> Reply {
    envelope(Some(builders::pack_any(
        message,
        &format!("kiapi.common.commands.{name}"),
    )))
}

fn error_reply(code: kiapi::common::ApiStatusCode, message: &str) -> Reply {
    Some(
        kiapi::common::ApiResponse {
            status: Some(status(code, message)),
            header: None,
            message: None,
        }
        .encode_to_vec(),
    )
}

fn doc() -> kiapi::common::types::DocumentSpecifier {
    kiapi::common::types::DocumentSpecifier {
        r#type: kiapi::common::types::DocumentType::DoctypePcb as i32,
        project: Some(kiapi::common::types::ProjectSpecifier {
            name: "mock".to_string(),
            path: if cfg!(windows) {
                r"C:\mock-project"
            } else {
                "/mock-project"
            }
            .to_string(),
        }),
        identifier: Some(
            kiapi::common::types::document_specifier::Identifier::BoardFilename(
                "b.kicad_pcb".to_string(),
            ),
        ),
    }
}

fn item() -> prost_types::Any {
    builders::pack_any(&kiapi::common::commands::Ping {}, "test.Item")
}

fn item_status(code: ItemStatusCode, message: &str) -> Option<ItemStatus> {
    Some(ItemStatus {
        code: code as i32,
        error_message: message.to_string(),
    })
}

fn created(
    request_status: ItemRequestStatus,
    results: Vec<(Option<ItemStatus>, Option<prost_types::Any>)>,
) -> Reply {
    reply(
        &kiapi::common::commands::CreateItemsResponse {
            header: None,
            status: request_status as i32,
            created_items: results
                .into_iter()
                .map(|(status, item)| kiapi::common::commands::ItemCreationResult { status, item })
                .collect(),
        },
        "CreateItemsResponse",
    )
}

fn updated(request_status: ItemRequestStatus, statuses: Vec<Option<ItemStatus>>) -> Reply {
    reply(
        &kiapi::common::commands::UpdateItemsResponse {
            header: None,
            status: request_status as i32,
            updated_items: statuses
                .into_iter()
                .map(|status| kiapi::common::commands::ItemUpdateResult { status, item: None })
                .collect(),
        },
        "UpdateItemsResponse",
    )
}

fn deleted(
    request_status: ItemRequestStatus,
    results: Vec<(Option<&str>, kiapi::common::commands::ItemDeletionStatus)>,
) -> Reply {
    reply(
        &kiapi::common::commands::DeleteItemsResponse {
            header: None,
            status: request_status as i32,
            deleted_items: results
                .into_iter()
                .map(|(id, status)| kiapi::common::commands::ItemDeletionResult {
                    id: id.map(|value| kiapi::common::types::Kiid {
                        value: value.to_string(),
                    }),
                    status: status as i32,
                })
                .collect(),
        },
        "DeleteItemsResponse",
    )
}

fn text<T>(result: anyhow::Result<T>) -> String {
    match result {
        Ok(_) => panic!("expected a failure"),
        Err(error) => format!("{error:#}"),
    }
}

#[test]
fn empty_requests_never_reach_the_wire() {
    let mock = spawn(|_| {
        error_reply(
            kiapi::common::ApiStatusCode::AsBadRequest,
            "must not be asked",
        )
    });
    let client = KiCadIpcClient::new(&mock.url);

    assert!(client
        .create_items_in_returning(doc(), vec![])
        .unwrap()
        .is_empty());
    client.update_items_in(doc(), vec![]).unwrap();
    client.delete_items_in(doc(), vec![]).unwrap();
    client.delete_items(vec![]).unwrap();

    assert_eq!(mock.requests.load(Ordering::SeqCst), 0);
}

#[test]
fn item_creation_refused_by_kicad_names_the_request_status() {
    let mock = spawn(|_| created(ItemRequestStatus::IrsDocumentNotFound, vec![]));
    let client = KiCadIpcClient::new(&mock.url);

    let error = text(client.create_items_in(doc(), vec![item()]));

    assert!(
        error.contains("item creation request failed with IRS_DOCUMENT_NOT_FOUND"),
        "{error}"
    );
}

#[test]
fn creating_nothing_is_a_failure_not_a_success() {
    let mock = spawn(|_| created(ItemRequestStatus::IrsOk, vec![]));
    let client = KiCadIpcClient::new(&mock.url);

    let error = text(client.create_items_in(doc(), vec![item(), item()]));

    assert!(error.contains("KiCad created no items"), "{error}");
    assert!(error.contains("2 requested"), "{error}");
}

#[test]
fn a_partly_created_batch_lists_each_rejection_and_is_not_success() {
    let mock = spawn(|_| {
        created(
            ItemRequestStatus::IrsOk,
            vec![
                (item_status(ItemStatusCode::IscOk, ""), Some(item())),
                (item_status(ItemStatusCode::IscInvalidData, "bad pad"), None),
                (item_status(ItemStatusCode::IscExisting, ""), None),
                (None, None),
            ],
        )
    });
    let client = KiCadIpcClient::new(&mock.url);

    let error = text(client.create_items_in(doc(), vec![item(), item(), item(), item()]));

    assert!(error.contains("created 1 of 4 requested items"), "{error}");
    assert!(
        error.contains("item 1: ISC_INVALID_DATA (bad pad)"),
        "{error}"
    );
    assert!(
        error.contains("item 2: ISC_EXISTING (no error message)"),
        "{error}"
    );
    assert!(
        error.contains("item 3: ISC_UNKNOWN (no error message)"),
        "{error}"
    );
}

#[test]
fn a_defaulted_status_counts_as_created_only_when_the_item_came_back() {
    let with_item = spawn(|_| created(ItemRequestStatus::IrsOk, vec![(None, Some(item()))]));
    let returned = KiCadIpcClient::new(&with_item.url)
        .create_items_in_returning(doc(), vec![item()])
        .unwrap();
    assert_eq!(returned.len(), 1);

    let without_item = spawn(|_| {
        created(
            ItemRequestStatus::IrsOk,
            vec![(item_status(ItemStatusCode::IscUnknown, ""), None)],
        )
    });
    let error = text(KiCadIpcClient::new(&without_item.url).create_items_in(doc(), vec![item()]));
    assert!(error.contains("created 0 of 1"), "{error}");
}

#[test]
fn an_ok_creation_without_an_echoed_item_succeeds_with_no_returned_ids() {
    let mock = spawn(|_| {
        created(
            ItemRequestStatus::IrsOk,
            vec![(item_status(ItemStatusCode::IscOk, ""), None)],
        )
    });
    let returned = KiCadIpcClient::new(&mock.url)
        .create_items_in_returning(doc(), vec![item()])
        .unwrap();
    assert!(returned.is_empty());
}

#[test]
fn item_update_failures_name_what_kicad_reported() {
    let refused = spawn(|_| updated(ItemRequestStatus::IrsDocumentNotFound, vec![]));
    let error = text(KiCadIpcClient::new(&refused.url).update_items_in(doc(), vec![item()]));
    assert!(
        error.contains("item update request failed with IRS_DOCUMENT_NOT_FOUND"),
        "{error}"
    );

    let immutable = spawn(|_| {
        updated(
            ItemRequestStatus::IrsOk,
            vec![item_status(ItemStatusCode::IscImmutable, "locked by API")],
        )
    });
    let error = text(KiCadIpcClient::new(&immutable.url).update_items_in(doc(), vec![item()]));
    assert!(
        error.contains("KiCad item update failed: locked by API (ISC_IMMUTABLE)"),
        "{error}"
    );

    let statusless = spawn(|_| updated(ItemRequestStatus::IrsOk, vec![None]));
    let error = text(KiCadIpcClient::new(&statusless.url).update_items_in(doc(), vec![item()]));
    assert!(
        error.contains("update result without item status"),
        "{error}"
    );

    let all_ok = spawn(|_| {
        updated(
            ItemRequestStatus::IrsOk,
            vec![
                item_status(ItemStatusCode::IscOk, ""),
                item_status(ItemStatusCode::IscOk, ""),
            ],
        )
    });
    KiCadIpcClient::new(&all_ok.url)
        .update_items_in(doc(), vec![item(), item()])
        .unwrap();
}

#[test]
fn deletion_results_are_validated_strictly_when_present() {
    use kiapi::common::commands::ItemDeletionStatus::{IdsImmutable, IdsOk};
    let ask = |ids: &[&str], results: Vec<(Option<&'static str>, _)>| {
        let mock = spawn(move |_| deleted(ItemRequestStatus::IrsOk, results.clone()));
        KiCadIpcClient::new(&mock.url)
            .delete_items_in(doc(), ids.iter().map(|id| id.to_string()).collect())
    };

    let error = text(ask(&["a", "a"], vec![]));
    assert!(error.contains("duplicate item identifiers"), "{error}");

    let error = text(ask(&["a", "b"], vec![(Some("a"), IdsOk)]));
    assert!(
        error.contains("1 deletion results for 2 requested"),
        "{error}"
    );

    let error = text(ask(&["a"], vec![(None, IdsOk)]));
    assert!(error.contains("without an item identifier"), "{error}");

    let error = text(ask(&["a"], vec![(Some("zzz"), IdsOk)]));
    assert!(error.contains("unexpected item 'zzz'"), "{error}");

    let error = text(ask(&["a"], vec![(Some("a"), IdsImmutable)]));
    assert!(
        error.contains("failed to delete item 'a': IDS_IMMUTABLE"),
        "{error}"
    );

    ask(&["a", "b"], vec![(Some("b"), IdsOk), (Some("a"), IdsOk)]).unwrap();
}

#[test]
fn deletion_refused_by_kicad_names_the_request_status() {
    let mock = spawn(|_| deleted(ItemRequestStatus::IrsDocumentNotFound, vec![]));
    let error = text(KiCadIpcClient::new(&mock.url).delete_items_in(doc(), vec!["a".into()]));
    assert!(
        error.contains("item deletion request failed with IRS_DOCUMENT_NOT_FOUND"),
        "{error}"
    );
}

#[test]
fn commit_identifiers_must_be_present_and_non_empty() {
    let missing = spawn(|_| {
        reply(
            &kiapi::common::commands::BeginCommitResponse { id: None },
            "BeginCommitResponse",
        )
    });
    let error = text(KiCadIpcClient::new(&missing.url).begin_commit());
    assert!(
        error.contains("BeginCommit without a commit identifier"),
        "{error}"
    );

    let empty = spawn(|_| {
        reply(
            &kiapi::common::commands::BeginCommitResponse {
                id: Some(kiapi::common::types::Kiid {
                    value: String::new(),
                }),
            },
            "BeginCommitResponse",
        )
    });
    let error = text(KiCadIpcClient::new(&empty.url).begin_commit());
    assert!(error.contains("empty commit identifier"), "{error}");
}

#[test]
fn a_commit_end_without_a_typed_payload_is_not_accepted() {
    let mock = spawn(|_| envelope(None));
    let error = text(KiCadIpcClient::new(&mock.url).push_commit("c1", "msg"));
    assert!(error.contains("EndCommit"), "{error}");
}

#[test]
fn wait_for_board_ready_retries_busy_and_gives_up_at_the_deadline() {
    let busy_then_ready = Arc::new(Mutex::new(2));
    let remaining = busy_then_ready.clone();
    let mock = spawn(move |_| {
        let mut left = remaining.lock().unwrap();
        if *left > 0 {
            *left -= 1;
            error_reply(kiapi::common::ApiStatusCode::AsBusy, "starting")
        } else {
            reply(
                &kiapi::common::commands::GetOpenDocumentsResponse {
                    documents: vec![doc()],
                },
                "GetOpenDocumentsResponse",
            )
        }
    });
    KiCadIpcClient::new(&mock.url)
        .wait_for_board_ready(Duration::from_secs(5))
        .expect("a busy KiCad that becomes ready is waited for");
    assert!(mock.requests.load(Ordering::SeqCst) >= 3);

    let always_busy = spawn(|_| error_reply(kiapi::common::ApiStatusCode::AsBusy, "still busy"));
    let error =
        text(KiCadIpcClient::new(&always_busy.url).wait_for_board_ready(Duration::from_millis(1)));
    assert!(
        error.contains("board readiness could not be established"),
        "{error}"
    );

    let bad = spawn(|_| error_reply(kiapi::common::ApiStatusCode::AsBadRequest, "no"));
    let before = std::time::Instant::now();
    let error = text(KiCadIpcClient::new(&bad.url).wait_for_board_ready(Duration::from_secs(30)));
    assert!(
        error.contains("board readiness could not be established"),
        "{error}"
    );
    assert!(
        before.elapsed() < Duration::from_secs(10),
        "a non-busy refusal must not be retried"
    );
    assert_eq!(bad.requests.load(Ordering::SeqCst), 1);
}

#[test]
fn a_status_error_keeps_its_code_and_falls_back_to_the_code_name() {
    let with_message = spawn(|_| error_reply(kiapi::common::ApiStatusCode::AsBadRequest, "nope"));
    let error = KiCadIpcClient::new(&with_message.url)
        .get_kicad_version()
        .unwrap_err();
    let status = ApiStatusError::from_error(&error).expect("typed status");
    assert_eq!(status.code_name, "AS_BAD_REQUEST");
    assert_eq!(status.message, "nope");
    assert!(!status.is_unsupported());

    let without = spawn(|_| error_reply(kiapi::common::ApiStatusCode::AsUnhandled, ""));
    let error = KiCadIpcClient::new(&without.url)
        .get_kicad_version()
        .unwrap_err();
    let status = ApiStatusError::from_error(&error).expect("typed status");
    assert_eq!(status.message, "AsUnhandled");
    assert!(status.is_unsupported());
}

#[test]
fn malformed_replies_are_outcome_unknown_never_success() {
    let garbage = spawn(|_| Some(vec![0xff, 0xff, 0xff, 0xff, 0xff]));
    let error = KiCadIpcClient::new(&garbage.url)
        .get_kicad_version()
        .unwrap_err();
    let unknown = error
        .downcast_ref::<IpcOutcomeUnknown>()
        .expect("outcome unknown");
    assert!(
        unknown.reason.contains("Failed to decode ApiResponse"),
        "{}",
        unknown.reason
    );

    let no_status = spawn(|_| {
        Some(
            kiapi::common::ApiResponse {
                status: None,
                header: None,
                message: None,
            }
            .encode_to_vec(),
        )
    });
    let error = KiCadIpcClient::new(&no_status.url)
        .get_kicad_version()
        .unwrap_err();
    let unknown = error
        .downcast_ref::<IpcOutcomeUnknown>()
        .expect("outcome unknown");
    assert!(
        unknown.reason.contains("missing its status"),
        "{}",
        unknown.reason
    );

    let strange = spawn(|_| {
        Some(
            kiapi::common::ApiResponse {
                status: Some(kiapi::common::ApiResponseStatus {
                    status: 9999,
                    error_message: String::new(),
                }),
                header: None,
                message: None,
            }
            .encode_to_vec(),
        )
    });
    let error = KiCadIpcClient::new(&strange.url)
        .get_kicad_version()
        .unwrap_err();
    let unknown = error
        .downcast_ref::<IpcOutcomeUnknown>()
        .expect("outcome unknown");
    assert!(
        unknown.reason.contains("unknown response status code"),
        "{}",
        unknown.reason
    );
}

#[test]
fn a_version_reply_without_a_version_is_refused() {
    let mock = spawn(|_| {
        reply(
            &kiapi::common::commands::GetVersionResponse { version: None },
            "GetVersionResponse",
        )
    });
    let error = text(KiCadIpcClient::new(&mock.url).get_kicad_version());
    assert!(error.contains("did not contain a KiCad version"), "{error}");
}

#[test]
fn ping_reports_an_error_status_as_a_request_that_may_have_arrived() {
    let mock = spawn(|_| error_reply(kiapi::common::ApiStatusCode::AsNotReady, "starting up"));
    match KiCadIpcClient::new(&mock.url).ping_outcome() {
        PingOutcome::RequestFailed { message } => {
            assert!(message.contains("starting up"), "{message}")
        }
        other => panic!("expected RequestFailed, got {other:?}"),
    }
}
