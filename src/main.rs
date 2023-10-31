use std::{ffi::OsStr, fs, path::Path};

use anyhow::Result;
use clap::Parser;
use config::Config;
use crossbeam_queue::ArrayQueue;
use kdam::{rayon::prelude::*, Bar, BarExt, TqdmParallelIterator};
use lazy_static::lazy_static;
use regex::Regex;
use reqwest::{blocking::Client, Proxy};
use retry::delay::{jitter, Exponential};
use scraper::{Html, Selector};

mod config;

/* https://techblog.willshouse.com/2012/01/03/most-common-user-agents */
const USER_AGENT: &str = "Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/118.0.0.0 Safari/537.36";
const BASE_URL: &str = "http://www.ptorrents.com";
const ADDR_URL: &str = "https://api.seeip.org";

#[derive(Debug, Parser)]
struct Args {
    #[arg(short, long, default_value = ".")]
    base_path: String,

    #[arg(short, long, default_value = "proxies.txt")]
    proxies_path: String,

    #[arg(short, long, default_value = USER_AGENT)]
    user_agent: String,
}

fn main() -> Result<()> {
    let args = Args::parse();
    let base_path = &args.base_path;
    let mut config = Config::load(base_path).unwrap_or_default();

    /* Step 1 */
    println!("Step 1: Checking Proxies...");
    let clients = fs::read_to_string(args.proxies_path)?
        .split('\n')
        .par_bridge()
        .map(String::from)
        .map(|proxy_scheme| {
            let proxy = Proxy::all(&proxy_scheme);

            proxy.map(|proxy| (proxy, proxy_scheme))
        })
        .filter_map(Result::ok)
        .map(|(proxy, proxy_scheme)| {
            let client = Client::builder()
                .proxy(proxy)
                .user_agent(USER_AGENT)
                .build();

            client.map(|client| (client, proxy_scheme))
        })
        .filter_map(Result::ok)
        .filter_map(check_proxy)
        .collect::<Vec<_>>();

    /* Step 2 */
    println!("Step 2: Getting max page number...");
    let max_pages = {
        /* Saving */
        let file = (BASE_URL.to_string(), format!("{base_path}/HTML/INDEX.HTML"));
        let contents = save_file(&clients[0], &file)?;

        /* Scraping */
        let html = Html::parse_document(&contents);
        let selector = Selector::parse("a.page-numbers").unwrap();
        let elements = html.select(&selector).collect::<Vec<_>>();
        let element = elements[elements.len() - 2];
        let texts = element.text().collect::<Vec<_>>();
        let text = texts.first().expect("Failed to find text");

        text.replace(',', "").parse()?
    };

    /* Step 3 */
    if max_pages > config.max_pages {
        let pages = (1..=max_pages)
            .map(|page| {
                let url = format!("{BASE_URL}/page/{page}");
                let path = format!("{base_path}/HTML/PAGES/{page}.HTML");
                (url, path)
            })
            .collect();
        let text = format!("Step 3: Saving {max_pages} pages to disk...");
        save_files(&clients, pages, max_pages, text)?;

        config.max_pages = max_pages;
        config.save(base_path)?;

        /* Step 4 */
        let mut bar = Bar::new(max_pages);
        bar.write(format!("Step 4: Scraping {max_pages} pages for entries..."))?;

        config.entries = (1..max_pages)
            .into_par_iter()
            .tqdm_with_bar(bar)
            .map(|page| (format!("{base_path}/HTML/PAGES/{page}.HTML"), ".html"))
            .map(scrape_files)
            .filter_map(Result::ok)
            .flatten()
            .collect();

        config.entries.sort();
        config.entries.dedup();
        config.save(base_path)?;
    } else {
        println!("Step 3: Saving {max_pages} pages to disk... (Skipped)");
        println!("Step 4: Scraping {max_pages} pages for entries... (Skipped)");
    }

    /* Step 5 */
    let max_entries = config.entries.len();
    let entries = config
        .entries
        .iter()
        .map(|entry| {
            let url = format!("{BASE_URL}/{entry}");
            let path = format!("{base_path}/HTML/ENTRIES/{entry}.HTML");
            (url, path)
        })
        .filter(|(_url, path)| fs::metadata(path).is_err())
        .collect::<Vec<_>>();

    let new_entries = entries.len();
    if new_entries > 0 {
        let text = format!("Step 5: Saving {max_entries} entries to disk... ({new_entries})");
        save_files(&clients, entries, new_entries, text)?;

        /* Step 6 */
        let mut bar = Bar::new(max_entries);
        let text = format!("Step 6: Scraping {max_entries} entries for torrents...");
        bar.write(text)?;

        config.torrents = config
            .entries
            .par_iter()
            .tqdm_with_bar(bar)
            .map(|entry| (format!("{base_path}/HTML/ENTRIES/{entry}.HTML"), ".torrent"))
            .map(scrape_files)
            .filter_map(Result::ok)
            .flatten()
            .collect();

        config.torrents.sort();
        config.torrents.dedup();
        config.save(base_path)?;
    } else {
        println!("Step 5: Saving {max_entries} entries to disk... (Skipped)");
        println!("Step 6: Scraping {max_entries} entries for torrents... (Skipped)");
    }

    /* Step 7 */
    let regex = Regex::new(r"^https://d\.ptorrents\.com/(.+)/\[ptorrents.com\]\.(.+)\.torrent$")?;
    let max_torrents = config.torrents.len();
    let torrents = config
        .torrents
        .into_iter()
        .filter_map(|haystack| {
            let Some(captures) = regex.captures(&haystack) else {
                return None;
            };

            let Some(path) = captures.get(1).map(|m| m.as_str()) else {
                return None;
            };

            let Some(name) = captures.get(2).map(|m| m.as_str()) else {
                return None;
            };

            let path = format!("{base_path}/TORRENT/{path}/{name}.TORRENT");

            Some((haystack, path))
        })
        .filter(|(_url, path)| fs::metadata(path).is_err())
        .collect::<Vec<_>>();

    let new_torrents = torrents.len();
    if new_torrents > 0 {
        let text = format!("Step 7: Saving {max_torrents} torrents to disk... ({new_torrents})");
        save_files(&clients, torrents, new_torrents, text)?;
    } else {
        println!("Step 7: Saving {max_torrents} torrents to disk... (Skipped)");
    }

    Ok(())
}

fn check_proxy((client, proxy): (Client, String)) -> Option<Client> {
    lazy_static! {
        static ref LOCAL_TEXT: String = reqwest::blocking::get(ADDR_URL).unwrap().text().unwrap();
    }

    let Ok(remote_response) = client.get(ADDR_URL).send() else {
        eprintln!("Failed to get response {proxy}");
        return None;
    };

    let Ok(remote_text) = remote_response.text() else {
        eprintln!("Failed to get response {proxy}");
        return None;
    };

    if remote_text == LOCAL_TEXT.as_str() {
        eprintln!("Failed to connect {proxy}");
        return None;
    }

    Some(client)
}

type File = (String, String);
fn save_files(clients: &Vec<Client>, files: Vec<File>, total: usize, text: String) -> Result<()> {
    let queue = ArrayQueue::new(total);
    let _ = files.into_par_iter().try_for_each(|msg| queue.push(msg));

    let mut bar = Bar::new(total);
    bar.desc = clients.len().to_string();
    bar.write(text)?;

    clients
        .into_par_iter()
        .for_each_with(bar, move |bar, client| {
            while let Some(msg) = queue.pop() {
                let _ = bar.update_to(total - queue.len());

                if let Err(error) = save_file(client, &msg) {
                    eprintln!("{error}");

                    queue.push(msg).unwrap();
                }
            }
        });

    Ok(())
}

fn save_file(client: &Client, (url, path): &File) -> Result<String> {
    let contents = get_text(client, url)?;

    if let Some(file_name) = Path::new(&path).file_name().and_then(OsStr::to_str) {
        let directory_path = path.replace(file_name, "");
        fs::create_dir_all(directory_path)?;
    };

    fs::write(path, &contents)?;

    Ok(contents)
}

fn scrape_files((path, pat): (String, &str)) -> Result<Vec<String>> {
    lazy_static! {
        static ref SELECTOR: Selector = Selector::parse("a[href]").unwrap();
    }

    let contents = fs::read_to_string(path)?;
    let html = Html::parse_document(&contents);
    let links = html
        .select(&SELECTOR)
        .filter_map(|e| e.value().attr("href"))
        .map(String::from)
        .filter(|s| s.ends_with(pat))
        .map(|s| s.replace(BASE_URL, ""))
        .collect();

    Ok(links)
}

fn get_text(client: &Client, url: &str) -> Result<String> {
    let iterable = Exponential::from_millis(100).map(jitter).take(10);
    let operation = |_| client.get(url).send();
    let response = retry::retry_with_index(iterable, operation)?;
    let text = response.text()?;

    Ok(text)
}
