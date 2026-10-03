use std::{collections::HashMap, error::Error, path::PathBuf};

use axum::{
    Router,
    extract::{Query, State},
    response::{Html, IntoResponse, Redirect, Response},
    routing::get,
};
use etcetera::BaseStrategy;
use serde::{Deserialize, Serialize};

// Configure all settings here.
// Note that the bangs including custom bangs are all (usually) cached in
// `~/.cache/wortel-redirect/bangs.json.br`
const HOST: &str = "0.0.0.0:5100";
const BANGS_JSON_SOURCE: &str =
    "https://raw.githubusercontent.com/kagisearch/bangs/refs/heads/main/data/bangs.json";
const DEFAULT_SEARCH_ENGINE: &str = "https://duckduckgo.com/?q={{{s}}}&ia=web";

fn custom_bangs(bangs: &mut HashMap<String, BangResult>) {
    // add your custom bangs here
    bangs.insert(
        "wortel".to_owned(),
        BangResult {
            domain: "http://localhost:5100/redirect".to_owned(),
            url: "http://localhost:5100/redirect".to_owned(),
        },
    );
}

fn get_bang_cache_folder() -> PathBuf {
    let mut path = etcetera::choose_base_strategy().unwrap().cache_dir();
    path.push("wortel-redirect");
    path
}

fn get_bang_cache_file() -> PathBuf {
    let mut path = get_bang_cache_folder();
    path.push("bangs.json.gz");
    path
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    tokio::runtime::Builder::new_current_thread()
        .enable_time()
        .enable_io()
        .build()?
        .block_on(async {
            // We will wait for our data before starting the server
            let mut bangs = fetch_bangs().await?;
            custom_bangs(&mut bangs);

            // I'm currently using this script directly, not behind caddy or anything.
            let app = Router::new()
                .route("/redirect", get(endpoint_handler))
                .with_state(bangs);
            let listener = tokio::net::TcpListener::bind(HOST).await?;

            println!("Ready! Listening on {HOST}");
            axum::serve(listener, app).await?;

            Ok(())
        })
}

// See also:
// https://github.com/kagisearch/bangs/#bang-format
// https://github.com/kagisearch/bangs/blob/main/data/bangs.schema.json
// TODO: flags, regex
#[derive(Serialize, Deserialize)]
struct KagiBang {
    /// Domain of the website.
    d: String,

    /// The domain of the actual website if the bang searches another website, if applicable. For use by snaps.
    ad: Option<String>,

    /// Url template for searches, with {{{s}}} as placeholder.
    u: String,

    /// Website name.
    s: String,

    /// Main trigger is `"!" + t`.
    t: String,

    /// Additional triggers next to `t`.
    #[serde(default)]
    ts: Vec<String>,
}

/// First checks for cached bangs, otherwise:
/// - get bangs from kagi
/// - add custom bangs
/// - save cache
async fn fetch_bangs() -> Result<HashMap<String, BangResult>, Box<dyn Error>> {
    let cache_file = get_bang_cache_file();
    if let Ok(content) = std::fs::read(&cache_file)
        && let Ok(decoded) =
            serde_json::from_reader(flate2::read::GzDecoder::new(content.as_slice()))
    {
        println!("Successfully got bangs from cache!");
        return Ok(decoded);
    }

    println!("Fetching bangs from '{BANGS_JSON_SOURCE}'...");
    let kagi_bangs: Vec<KagiBang> = reqwest::get(BANGS_JSON_SOURCE).await?.json().await?;

    let mut result = HashMap::<String, BangResult>::new();

    for bang in kagi_bangs {
        // extracting 'true' url, this is relevant for bangs like `!cpp`.
        let mut url = bang.u;
        if bang.d == "kagi.com" {
            if bang.s.starts_with("Kagi ") {
                // Exclude any kagi specific bangs. To use these you need an account with kagi.
                continue;
            }

            let Some(search) = url.strip_prefix("/search?q=") else {
                panic!();
            };

            if !search.contains("{{{s}}}") {
                panic!();
            }

            url = DEFAULT_SEARCH_ENGINE.replace("{{{s}}}", search);
        }

        let bang_result = BangResult {
            domain: "https://".to_owned() + &bang.ad.unwrap_or(bang.d),
            url,
        };

        result.insert(bang.t, bang_result.clone());
        for t in bang.ts {
            result.insert(t, bang_result.clone());
        }
    }

    // TODO: make this whole thing less akward
    use std::io::Read;
    let encoded = serde_json::to_vec(&result)?;
    let mut compressed = Vec::new();
    flate2::read::GzEncoder::new(encoded.as_slice(), flate2::Compression::best())
        .read_to_end(&mut compressed)?;

    std::fs::create_dir_all(get_bang_cache_folder())?;
    std::fs::write(cache_file, compressed)?;
    println!("Done fetching bangs, wrote cache!");

    Ok(result)
}

#[derive(Serialize, Deserialize, Clone)]
struct BangResult {
    /// Used if query with bang removed is empty.
    domain: String,

    /// Used if `domain` isn't used.
    url: String,
}

#[derive(Deserialize)]
struct QueryParams {
    q: Option<String>,
}

async fn endpoint_handler(
    params: Query<QueryParams>,
    bangs: State<HashMap<String, BangResult>>,
) -> Response {
    let Some(q) = &params.q else {
        // Serve static site with stuffies.
        return Html(include_str!("redirect.html")).into_response();
    };

    let mut search_engine = None;
    let mut parts: Vec<_> = q
        .split(' ')
        .filter(|part| !part.bytes().all(|b| b.is_ascii_whitespace()))
        .collect();

    for i in 0..parts.len() {
        if parts[i].starts_with('!') {
            let Some(bang) = bangs.get(&parts[i][1..]) else {
                continue;
            };

            parts.remove(i);

            if parts.is_empty() {
                search_engine = Some(bang.domain.clone());
            } else {
                search_engine = Some(bang.url.clone());
            }

            break;
        }
    }

    // TODO: flags
    let param = urlencoding::encode(&parts.join(" ")).replace("%20", "+");
    let redirect_url = search_engine
        .unwrap_or_else(|| DEFAULT_SEARCH_ENGINE.to_owned())
        .replace("{{{s}}}", &param);

    Redirect::to(&redirect_url).into_response()
}
