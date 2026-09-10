//! Live shadow: the same provider request through both transports, against a
//! real endpoint.
//!
//! This is the ADR-0200 P2 evidence run. It is an example rather than a test
//! because it needs a network and a credential, and neither belongs in CI:
//!
//! ```text
//! DEEPSEEK_API_KEY=... cargo run --release -p muta-llm-client \
//!   --features net-transport --example live_shadow -- \
//!   --base-url https://api.deepseek.com/v1/responses \
//!   --model deepseek-v4.1-flash-expires-on-0910 \
//!   --prompt "Say hello in five words."
//! ```
//!
//! Output: the two event streams compared, plus the owned transport's derived
//! timings. The key is never printed.

#![allow(clippy::expect_used, clippy::unwrap_used)]

use std::sync::{Arc, Mutex};

use futures::StreamExt;
use muta_contracts::{
    InstructionBundle, Message, ModelRequest, Provider, ProviderStreamEvent, Role,
};
use muta_llm_client::{Client, Egress, MutaNetEgress, OpenAiResponsesProvider, ReqwestEgress};
use netune_trace::{RequestTrace, derive};

struct Options {
    base_url: String,
    model: String,
    prompt: String,
    key: String,
    repeat: usize,
    via_env: bool,
}

fn options() -> Options {
    let mut base_url = "https://api.deepseek.com/v1/responses".to_string();
    let mut model = "deepseek-v4.1-flash-expires-on-0910".to_string();
    let mut prompt = "Say hello in five words.".to_string();
    let mut repeat = 1usize;
    let mut via_env = false;
    let mut args = std::env::args().skip(1);
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--base-url" => base_url = args.next().expect("--base-url value"),
            "--model" => model = args.next().expect("--model value"),
            "--prompt" => prompt = args.next().expect("--prompt value"),
            "--repeat" => repeat = args.next().expect("--repeat value").parse().expect("count"),
            // Exercise the environment-driven selection instead of explicit
            // egress construction.
            "--via-env" => via_env = true,
            other => {
                eprintln!("unknown argument: {other}");
                std::process::exit(2);
            }
        }
    }
    let key = std::env::var("DEEPSEEK_API_KEY")
        .or_else(|_| std::env::var("MUTA_LIVE_KEY"))
        .expect("set DEEPSEEK_API_KEY (or MUTA_LIVE_KEY)");
    Options {
        base_url,
        model,
        prompt,
        key,
        repeat,
        via_env,
    }
}

fn request(prompt: &str) -> ModelRequest {
    ModelRequest::with_instructions_and_tools(
        InstructionBundle::default(),
        vec![Message::new(Role::User, prompt)],
        &[],
    )
}

/// `None` means "let the environment choose" ([`Client::new`]).
async fn run(options: &Options, egress: Option<Arc<dyn Egress>>) -> Vec<ProviderStreamEvent> {
    let mut provider = OpenAiResponsesProvider::from_static_key(
        options.key.clone(),
        options.model.clone(),
        &options.base_url,
    );
    provider.client = match egress {
        Some(egress) => Client::with_egress(egress),
        None => Client::new(),
    };
    let stream = provider
        .stream_chat_events(request(&options.prompt))
        .await
        .expect("stream opens");
    stream
        .collect::<Vec<_>>()
        .await
        .into_iter()
        .map(|item| item.expect("event"))
        .collect()
}

/// The event-kind sequence with consecutive text deltas collapsed, so two calls
/// that chunk differently still compare equal.
fn shape(events: &[ProviderStreamEvent]) -> Vec<&'static str> {
    let mut out: Vec<&'static str> = Vec::new();
    for event in events {
        let kind = match event {
            ProviderStreamEvent::TextDelta(_) => "text",
            ProviderStreamEvent::ReasoningDelta(_) => "reasoning",
            ProviderStreamEvent::ToolCallDelta { .. } => "tool",
            ProviderStreamEvent::Usage(_) => "usage",
            ProviderStreamEvent::Completed(_) => "completed",
            ProviderStreamEvent::ModelCatalogEtag(_) => "etag",
        };
        if out.last() != Some(&kind) {
            out.push(kind);
        }
    }
    out
}

fn text_of(events: &[ProviderStreamEvent]) -> String {
    events
        .iter()
        .filter_map(|event| match event {
            ProviderStreamEvent::TextDelta(text) => Some(text.as_str()),
            _ => None,
        })
        .collect()
}

#[tokio::main(flavor = "multi_thread")]
async fn main() {
    let options = options();
    println!(
        "live shadow: {} @ {} (prompt: {:?}, repeat {})",
        options.model, options.base_url, options.prompt, options.repeat
    );
    if options.via_env {
        println!(
            "  selection: MUTA_EGRESS={:?} → {:?}",
            std::env::var("MUTA_EGRESS").unwrap_or_default(),
            muta_llm_client::client::egress_choice(
                std::env::var("MUTA_EGRESS").ok().as_deref(),
                std::env::var("MUTA_PROXY").ok().as_deref(),
            )
        );
    }

    let mut agreements = 0usize;
    let mut attempts = 0usize;
    for iteration in 1..=options.repeat {
        if options.repeat > 1 {
            println!("--- iteration {iteration}/{} ---", options.repeat);
        }
        let agreed = one_round(&options).await;
        attempts += 1;
        agreements += usize::from(agreed);
    }
    if options.repeat > 1 {
        println!("live shadow: completed + identical text {agreements}/{attempts}");
    }
}

async fn one_round(options: &Options) -> bool {
    let trace_slot: Arc<Mutex<Option<RequestTrace>>> = Arc::new(Mutex::new(None));
    let slot = Arc::clone(&trace_slot);
    let owned_egress = MutaNetEgress::new()
        .expect("platform TLS")
        .with_trace_sink(Arc::new(move |trace| {
            *slot.lock().unwrap_or_else(|error| error.into_inner()) = Some(trace);
        }));
    let reqwest_events = run(
        options,
        Some(Arc::new(ReqwestEgress::new(reqwest::Client::new()))),
    )
    .await;
    // With `--via-env` the owned run goes through `Client::new()`, which is the
    // path the daemon takes when `MUTA_EGRESS=net` is set.
    let owned_events = if options.via_env {
        run(options, None).await
    } else {
        run(options, Some(Arc::new(owned_egress))).await
    };

    let reqwest_text = text_of(&reqwest_events);
    let owned_text = text_of(&owned_events);
    println!(
        "reqwest: {} events, {} chars of text",
        reqwest_events.len(),
        reqwest_text.chars().count()
    );
    println!(
        "owned:   {} events, {} chars of text",
        owned_events.len(),
        owned_text.chars().count()
    );
    // A live endpoint is stochastic: two calls legitimately differ in wording,
    // and a reasoning model may or may not think before answering. The property
    // a transport must satisfy is therefore *completion*: a well-formed stream
    // that ends in a terminal event. Text equality is reported, not asserted.
    let reqwest_shape = shape(&reqwest_events);
    let owned_shape = shape(&owned_events);
    let reqwest_completed = reqwest_shape.last() == Some(&"completed");
    let owned_completed = owned_shape.last() == Some(&"completed");
    println!(
        "completed: reqwest={reqwest_completed} owned={owned_completed}          (shapes {reqwest_shape:?} / {owned_shape:?})"
    );
    println!("  text equal: {}", reqwest_text == owned_text);
    println!("  reqwest text: {reqwest_text:?}");
    println!("  owned text:   {owned_text:?}");

    if let Some(trace) = trace_slot.lock().expect("trace slot").take() {
        let derived = derive(&trace);
        println!("--- owned transport trace ---");
        for (name, reading) in [
            ("dns_us", derived.dns_us),
            ("tcp_us", derived.tcp_us),
            ("tls_us", derived.tls_us),
            ("ttfb_us", derived.ttfb_us),
            ("first_byte_us", derived.first_byte_us),
            ("e2e_us", derived.e2e_us),
            ("rtt_us", derived.rtt_us),
        ] {
            match reading.value() {
                Some(value) => println!("  {name:<14} {value} µs"),
                None => println!("  {name:<14} – ({:?})", reading.validity()),
            }
        }
        println!("  events         {}", trace.log.len());
        println!("  bytes read     {}", trace.log.bytes_read());
        println!("  bytes written  {}", trace.log.bytes_written());
    }
    owned_completed && owned_text == reqwest_text
}
