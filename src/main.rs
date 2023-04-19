use std::{
    collections::{BTreeMap, HashMap},
    io::Write,
    time::Duration,
};

use chrono::Datelike;
use futures::future::TryFutureExt;

#[derive(Debug)]
struct Channel {
    url: String,
    author: String,
    group: String,
}

#[derive(Debug, Clone)]
struct FeedsItem {
    title: String,
    author: String,
    date: chrono::DateTime<chrono::FixedOffset>,
    url: String,
    group: String,
}

fn parser_rss(feed: rss::Channel, channel: &Channel) -> Vec<FeedsItem> {
    let mut feeds = Vec::new();
    for item in feed.items {
        let title = item.title.unwrap_or("".to_string());
        let date = item.pub_date.expect("error format!");
        let date = match diligent_date_parser::parse_date(date.as_str()) {
            Some(date) => date,
            None => {
                println!(
                    "error on parsing date `{}`, at parsering {}",
                    date.as_str(),
                    channel.url
                );
                continue;
            }
        };
        feeds.push(FeedsItem {
            title,
            author: channel.author.to_string(),
            date,
            url: item.link.unwrap(),
            group: channel.group.to_string(),
        })
    }
    return feeds;
}

fn parser_atom(feed: atom_syndication::Feed, channel: &Channel) -> Vec<FeedsItem> {
    let mut feeds = Vec::new();
    for item in feed.entries() {
        let title = item.title().to_string();
        let date = item.published().unwrap_or(item.updated());
        feeds.push(FeedsItem {
            title,
            author: channel.author.to_string(),
            date: date.clone(),
            url: item.links[0].href.clone(),
            group: channel.group.to_string(),
        })
    }
    return feeds;
}

async fn fetch_feed(channels: &Vec<Channel>) -> Vec<FeedsItem> {
    let mut contents = Vec::new();

    let timeout = Duration::from_secs(20);
    let client = reqwest::ClientBuilder::default()
        .timeout(timeout)
        .build()
        .unwrap();

    for i in 0..channels.len() {
        let channel = &channels[i];
        println!("fetching {}", channel.url);

        let query = client.get(&channel.url).build().unwrap();
        let clone_query = query.try_clone().unwrap();
        let content = client
            .execute(query)
            .or_else(|_| {
                println!("retry {}", channel.url);
                client.execute(clone_query)
            })
            .and_then(move |s| {
                println!("finish {}", channel.url);
                s.bytes()
            });
        contents.push(content);
    }
    let contents = futures::future::join_all(contents).await;

    let mut feeds = Vec::new();

    for i in 0..channels.len() {
        let channel = &channels[i];
        let content = match contents[i].as_ref() {
            Ok(content) => content,
            Err(err) => {
                println!("error on fetching {}: {:#?}", &channel.url, err);
                continue;
            }
        };
        let read_buf = &content[..];
        match rss::Channel::read_from(read_buf) {
            Ok(content) => {
                feeds.append(&mut parser_rss(content, channel));
            }
            Err(_) => match atom_syndication::Feed::read_from(read_buf) {
                Ok(content) => {
                    feeds.append(&mut parser_atom(content, channel));
                }
                Err(_) => {
                    println!("parse error at {}", channel.url);
                }
            },
        }
    }
    return feeds;
}

fn get_channels(opml_file: opml::OPML) -> Vec<Channel> {
    let mut channels = Vec::new();

    for item in opml_file.body.outlines {
        match item.r#type {
            Some(ref outline_type) => {
                if outline_type != "rss" {
                    panic!("type in group should be `rss`.")
                }
                channels.push(Channel {
                    url: item.xml_url.unwrap(),
                    author: item.title.unwrap(),
                    group: "".to_string(),
                });
            }
            None => {
                let group_name = item.text;
                println!("group = {}", group_name);

                for item in item.outlines {
                    if item.r#type.as_ref().unwrap() != "rss" {
                        panic!("type in group should be `rss`.")
                    }
                    channels.push(Channel {
                        url: item.xml_url.unwrap(),
                        author: item.title.unwrap(),
                        group: group_name.to_string(),
                    });
                }
            }
        }
    }
    return channels;
}

fn split_by_group(feeds: &Vec<FeedsItem>) -> HashMap<String, Vec<FeedsItem>> {
    let mut s: HashMap<String, Vec<FeedsItem>> = HashMap::new();

    let now = chrono::Utc::now();
    let past_year = now.with_year(now.year() - 1).unwrap();

    for feed in feeds {
        let mut keys = vec!["".to_string()];
        if !feed.group.is_empty() {
            keys.push(feed.group.clone());
        }
        if feed.date.signed_duration_since(past_year).num_seconds() >= 0 {
            keys.push("this-year".to_string());
        }

        for k in keys {
            let v = &mut s.entry(k).or_insert(vec![]);
            v.push(feed.clone());
        }
    }
    return s;
}

fn generate_md(list: &Vec<FeedsItem>) -> String {
    if list.is_empty() {
        return "".to_string();
    }

    let mut s = BTreeMap::new();
    for item in list {
        let v = &mut s.entry(item.date.year()).or_insert(vec![]);
        v.push(item);
    }

    let fmt_year = |item: &FeedsItem| std::format!("# {}", item.date.year());

    let fmt_item = |item: &FeedsItem| {
        std::format!(
            "{}, @{}, [{}]({})",
            item.date.format("%Y-%m-%d"),
            item.author,
            item.title,
            item.url
        )
    };

    let mut buf = Vec::new();
    buf.push(fmt_year(&list[0]));

    for i in 1..list.len() {
        let item = &list[i];
        if item.date.year() != list[i - 1].date.year() {
            buf.push(fmt_year(&item));
        }
        buf.push(fmt_item(&item));
    }
    return buf.join("\n\n");
}

#[tokio::main]
async fn main() {
    let args: Vec<String> = std::env::args().collect();
    if args.len() < 3 {
        println!("useage: ga-rss <opml> <md-dir>");
        return;
    }
    let opml_path = std::path::PathBuf::from(&args[1]);
    let md_path = std::path::PathBuf::from(&args[2]);

    let mut file = std::fs::File::open(opml_path).unwrap();
    let opml_file = opml::OPML::from_reader(&mut file).unwrap();

    let channels = get_channels(opml_file);
    let mut feeds = fetch_feed(&channels).await;
    feeds.sort_by_key(|f| f.date);
    feeds.reverse();

    let s = split_by_group(&feeds);
    // println!("s = {:#?}", s);

    for (k, v) in &s {
        let group = if k.is_empty() {
            "all".to_string()
        } else {
            k.to_string()
        };
        let mut path = md_path.clone();
        path.push(std::format!("{}.md", group));
        let mut output = std::fs::File::create(path).unwrap();
        let doc = generate_md(&v);
        output.write(doc.as_bytes()).unwrap();
    }
}
