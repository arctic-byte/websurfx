//! This module handles the search route of the search engine website.

use crate::{
    cache::cacher::RedisCache,
    config::parser::Config,
    handler::paths::{file_path, FileType},
    models::{aggregation_models::SearchResults, engine_models::EngineHandler},
    results::aggregator::aggregate,
};
use actix_web::{get, web, HttpRequest, HttpResponse};
use handlebars::Handlebars;
use regex::Regex;
use serde::Deserialize;
use std::{
    fs::{read_to_string, File},
    io::{BufRead, BufReader, Read},
};
use tokio::join;

// ---- Constants ----
/// Initialize redis cache connection once and store it on the heap.
static REDIS_CACHE: async_once_cell::OnceCell<RedisCache> = async_once_cell::OnceCell::new();

/// A named struct which deserializes all the user provided search parameters and stores them.
#[derive(Deserialize)]
pub struct SearchParams {
    /// It stores the search parameter option `q` (or query in simple words)
    /// of the search url.
    q: Option<String>,
    /// It stores the search parameter `page` (or pageno in simple words)
    /// of the search url.
    page: Option<u32>,
    /// It stores the search parameter `safesearch` (or safe search level in simple words) of the
    /// search url.
    safesearch: Option<u8>,
}

/// Handles the route of index page or main page of the `websurfx` meta search engine website.
#[get("/")]
pub async fn index(
    hbs: web::Data<Handlebars<'_>>,
    config: web::Data<Config>,
) -> Result<HttpResponse, Box<dyn std::error::Error>> {
    let page_content: String = hbs.render("index", &config.style).unwrap();
    Ok(HttpResponse::Ok().body(page_content))
}

/// Handles the route of any other accessed route/page which is not provided by the
/// website essentially the 404 error page.
pub async fn not_found(
    hbs: web::Data<Handlebars<'_>>,
    config: web::Data<Config>,
) -> Result<HttpResponse, Box<dyn std::error::Error>> {
    let page_content: String = hbs.render("404", &config.style)?;

    Ok(HttpResponse::Ok()
        .content_type("text/html; charset=utf-8")
        .body(page_content))
}

/// A named struct which is used to deserialize the cookies fetched from the client side.
#[allow(dead_code)]
#[derive(Deserialize)]
struct Cookie<'a> {
    /// It stores the theme name used in the website.
    theme: &'a str,
    /// It stores the colorscheme name used for the website theme.
    colorscheme: &'a str,
    /// It stores the user selected upstream search engines selected from the UI.
    engines: Vec<&'a str>,
}

/// Handles the route of search page of the `websurfx` meta search engine website and it takes
/// two search url parameters `q` and `page` where `page` parameter is optional.
///
/// # Example
///
/// ```bash
/// curl "http://127.0.0.1:8080/search?q=sweden&page=1"
/// ```
///
/// Or
///
/// ```bash
/// curl "http://127.0.0.1:8080/search?q=sweden"
/// ```
#[get("/search")]
pub async fn search(
    hbs: web::Data<Handlebars<'_>>,
    req: HttpRequest,
    config: web::Data<Config>,
) -> Result<HttpResponse, Box<dyn std::error::Error>> {
    let params = web::Query::<SearchParams>::from_query(req.query_string())?;
    match &params.q {
        Some(query) => {
            if query.trim().is_empty() {
                return Ok(HttpResponse::Found()
                    .insert_header(("location", "/"))
                    .finish());
            }
            let page = match &params.page {
                Some(page) => *page,
                None => 1,
            };

            let safe_search: u8 = match config.safe_search {
                3..=4 => config.safe_search,
                _ => match &params.safesearch {
                    Some(safesearch) => match safesearch {
                        0..=2 => *safesearch,
                        _ => 1,
                    },
                    None => config.safe_search,
                },
            };

            let (_, results, _) = join!(
                results(
                    format!(
                        "http://{}:{}/search?q={}&page={}&safesearch={}",
                        config.binding_ip,
                        config.port,
                        query,
                        page - 1,
                        safe_search
                    ),
                    &config,
                    query,
                    page - 1,
                    req.clone(),
                    safe_search
                ),
                results(
                    format!(
                        "http://{}:{}/search?q={}&page={}&safesearch={}",
                        config.binding_ip, config.port, query, page, safe_search
                    ),
                    &config,
                    query,
                    page,
                    req.clone(),
                    safe_search
                ),
                results(
                    format!(
                        "http://{}:{}/search?q={}&page={}&safesearch={}",
                        config.binding_ip,
                        config.port,
                        query,
                        page + 1,
                        safe_search
                    ),
                    &config,
                    query,
                    page + 1,
                    req.clone(),
                    safe_search
                )
            );

            let page_content: String = hbs.render("search", &results?)?;
            Ok(HttpResponse::Ok().body(page_content))
        }
        None => Ok(HttpResponse::Found()
            .insert_header(("location", "/"))
            .finish()),
    }
}

/// Fetches the results for a query and page. It First checks the redis cache, if that
/// fails it gets proper results by requesting from the upstream search engines.
///
/// # Arguments
///
/// * `url` - It takes the url of the current page that requested the search results for a
/// particular search query.
/// * `config` - It takes a parsed config struct.
/// * `query` - It takes the page number as u32 value.
/// * `req` - It takes the `HttpRequest` struct as a value.
///
/// # Error
///
/// It returns the `SearchResults` struct if the search results could be successfully fetched from
/// the cache or from the upstream search engines otherwise it returns an appropriate error.
async fn results(
    url: String,
    config: &Config,
    query: &str,
    page: u32,
    req: HttpRequest,
    safe_search: u8,
) -> Result<SearchResults, Box<dyn std::error::Error>> {
    // Initialize redis cache connection struct
    let mut redis_cache: RedisCache = REDIS_CACHE
        .get_or_init(async {
            // Initialize redis cache connection pool only one and store it in the heap.
            RedisCache::new(&config.redis_url, 5).await.unwrap()
        })
        .await
        .clone();
    // fetch the cached results json.
    let cached_results_json: Result<String, error_stack::Report<crate::cache::error::PoolError>> =
        redis_cache.clone().cached_json(&url).await;
    // check if fetched cache results was indeed fetched or it was an error and if so
    // handle the data accordingly.
    match cached_results_json {
        Ok(results) => Ok(serde_json::from_str::<SearchResults>(&results)?),
        Err(_) => {
            if safe_search == 4 {
                let mut results: SearchResults = SearchResults::default();
                let mut _flag: bool =
                    is_match_from_filter_list(file_path(FileType::BlockList)?, query)?;
                _flag = !is_match_from_filter_list(file_path(FileType::AllowList)?, query)?;

                if _flag {
                    results.set_disallowed();
                    results.add_style(&config.style);
                    results.set_page_query(query);
                    redis_cache
                        .cache_results(&serde_json::to_string(&results)?, &url)
                        .await?;
                    return Ok(results);
                }
            }

            // check if the cookie value is empty or not if it is empty then use the
            // default selected upstream search engines from the config file otherwise
            // parse the non-empty cookie and grab the user selected engines from the
            // UI and use that.
            let mut results: SearchResults = match req.cookie("appCookie") {
                Some(cookie_value) => {
                    let cookie_value: Cookie<'_> =
                        serde_json::from_str(cookie_value.name_value().1)?;

                    let engines: Vec<EngineHandler> = cookie_value
                        .engines
                        .iter()
                        .filter_map(|name| EngineHandler::new(name))
                        .collect();

                    aggregate(
                        query,
                        page,
                        config.aggregator.random_delay,
                        config.debug,
                        &engines,
                        config.request_timeout,
                        safe_search,
                    )
                    .await?
                }
                None => {
                    aggregate(
                        query,
                        page,
                        config.aggregator.random_delay,
                        config.debug,
                        &config.upstream_search_engines,
                        config.request_timeout,
                        safe_search,
                    )
                    .await?
                }
            };
            if results.engine_errors_info().is_empty() && results.results().is_empty() {
                results.set_filtered();
            }
            results.add_style(&config.style);
            redis_cache
                .cache_results(&serde_json::to_string(&results)?, &url)
                .await?;
            Ok(results)
        }
    }
}

/// A helper function which checks whether the search query contains any keywords which should be
/// disallowed/allowed based on the regex based rules present in the blocklist and allowlist files.
fn is_match_from_filter_list(
    file_path: &str,
    query: &str,
) -> Result<bool, Box<dyn std::error::Error>> {
    let mut flag = false;
    let mut reader = BufReader::new(File::open(file_path)?);
    for line in reader.by_ref().lines() {
        let re = Regex::new(&line?)?;
        if re.is_match(query) {
            flag = true;
            break;
        }
    }
    Ok(flag)
}

/// Handles the route of robots.txt page of the `websurfx` meta search engine website.
#[get("/robots.txt")]
pub async fn robots_data(_req: HttpRequest) -> Result<HttpResponse, Box<dyn std::error::Error>> {
    let page_content: String =
        read_to_string(format!("{}/robots.txt", file_path(FileType::Theme)?))?;
    Ok(HttpResponse::Ok()
        .content_type("text/plain; charset=ascii")
        .body(page_content))
}

/// Handles the route of about page of the `websurfx` meta search engine website.
#[get("/about")]
pub async fn about(
    hbs: web::Data<Handlebars<'_>>,
    config: web::Data<Config>,
) -> Result<HttpResponse, Box<dyn std::error::Error>> {
    let page_content: String = hbs.render("about", &config.style)?;
    Ok(HttpResponse::Ok().body(page_content))
}

/// Handles the route of settings page of the `websurfx` meta search engine website.
#[get("/settings")]
pub async fn settings(
    hbs: web::Data<Handlebars<'_>>,
    config: web::Data<Config>,
) -> Result<HttpResponse, Box<dyn std::error::Error>> {
    let page_content: String = hbs.render("settings", &config.style)?;
    Ok(HttpResponse::Ok().body(page_content))
}
