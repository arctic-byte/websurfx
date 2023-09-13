//! This main library module provides the functionality to provide and handle the Tcp server
//! and register all the routes for the `websurfx` meta search engine website.

#![forbid(unsafe_code, clippy::panic)]
#![deny(missing_docs, clippy::missing_docs_in_private_items, clippy::perf)]
#![warn(clippy::cognitive_complexity, rust_2018_idioms)]

pub mod cache;
pub mod config;
pub mod engines;
pub mod handler;
pub mod results;
pub mod server;

use std::net::TcpListener;

use crate::server::routes;

use actix_cors::Cors;
use actix_files as fs;
use actix_governor::{Governor, GovernorConfigBuilder};
use actix_web::{dev::Server, http::header, middleware::Logger, web, App, HttpServer};
use config::parser::Config;
use handlebars::Handlebars;
use handler::paths::{file_path, FileType};

/// Runs the web server on the provided TCP listener and returns a `Server` instance.
///
/// # Arguments
///
/// * `listener` - A `TcpListener` instance representing the address and port to listen on.
///
/// # Returns
///
/// Returns a `Result` containing a `Server` instance on success, or an `std::io::Error` on failure.
///
/// # Example
///
/// ```rust
/// use std::net::TcpListener;
/// use websurfx::{config::parser::Config, run};
///
/// let config = Config::parse(true).unwrap();
/// let listener = TcpListener::bind("127.0.0.1:8080").expect("Failed to bind address");
/// let server = run(listener,config).expect("Failed to start server");
/// ```
pub fn run(listener: TcpListener, config: Config) -> std::io::Result<Server> {
    let mut handlebars: Handlebars<'_> = Handlebars::new();

    let public_folder_path: &str = file_path(FileType::Theme)?;

    handlebars
        .register_templates_directory(".html", format!("{}/templates", public_folder_path))
        .unwrap();

    let handlebars_ref: web::Data<Handlebars<'_>> = web::Data::new(handlebars);

    let cloned_config_threads_opt: u8 = config.threads;

    let server = HttpServer::new(move || {
        let cors: Cors = Cors::default()
            .allow_any_origin()
            .allowed_methods(vec!["GET"])
            .allowed_headers(vec![
                header::ORIGIN,
                header::CONTENT_TYPE,
                header::REFERER,
                header::COOKIE,
            ]);

        App::new()
            .wrap(Logger::default()) // added logging middleware for logging.
            .app_data(handlebars_ref.clone())
            .app_data(web::Data::new(config.clone()))
            .wrap(cors)
            .wrap(Governor::new(
                &GovernorConfigBuilder::default()
                    .per_second(config.rate_limiter.time_limit as u64)
                    .burst_size(config.rate_limiter.number_of_requests as u32)
                    .finish()
                    .unwrap(),
            ))
            // Serve images and static files (css and js files).
            .service(
                fs::Files::new("/static", format!("{}/static", public_folder_path))
                    .show_files_listing(),
            )
            .service(
                fs::Files::new("/images", format!("{}/images", public_folder_path))
                    .show_files_listing(),
            )
            .service(routes::robots_data) // robots.txt
            .service(routes::index) // index page
            .service(routes::search) // search page
            .service(routes::about) // about page
            .service(routes::settings) // settings page
            .default_service(web::route().to(routes::not_found)) // error page
    })
    .workers(cloned_config_threads_opt as usize)
    // Start server on 127.0.0.1 with the user provided port number. for example 127.0.0.1:8080.
    .listen(listener)?
    .run();
    Ok(server)
}
