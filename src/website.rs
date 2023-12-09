use crate::NPage;
use crate::BUFFER;
use compact_str::CompactString;
use indexmap::IndexMap;
use pyo3::prelude::*;
use spider::tokio::task::JoinHandle;
use spider::utils::shutdown;
use std::time::Duration;

/// a website holding the inner spider::website::Website from Rust fit for python.
#[pyclass]
pub struct Website {
  /// the website from spider.
  inner: spider::website::Website,
  /// spawned subscription handles.
  subscription_handles: IndexMap<u32, JoinHandle<()>>,
  /// spawned crawl handles.
  crawl_handles: IndexMap<u32, JoinHandle<()>>,
  /// do not convert content to UT8.
  raw_content: bool,
  /// the data collected.
  collected_data: Box<Vec<u8>>,
  /// is the crawl running in the background.
  running_in_background: bool, // /// the file handle for storing data
                               // file_handle: Option<spider::tokio::fs::File>,
}

struct PageEvent {
  pub page: NPage,
}

#[pymethods]
impl Website {
  /// a new website.
  #[new]
  pub fn new(url: String, raw_content: Option<bool>) -> Self {
    Website {
      inner: spider::website::Website::new(&url),
      subscription_handles: IndexMap::new(),
      crawl_handles: IndexMap::new(),
      raw_content: raw_content.unwrap_or_default(),
      collected_data: Box::new(Vec::new()),
      running_in_background: false, // file_handle: None,
    }
  }

  /// Get the crawl status.
  pub fn status(&self) -> String {
    self.inner.get_status().to_string()
  }

  // /// store data to memory for disk storing. This will create the path if not exist and defaults to ./storage.
  // pub async fn export_jsonl_data(&self, export_path: Option<String>) -> std::io::Result<()> {
  //   use spider::tokio::io::AsyncWriteExt;
  //   let file = match export_path {
  //     Some(p) => {
  //       let base_dir = p
  //         .split("/")
  //         .into_iter()
  //         .map(|f| {
  //           if f.contains(".") {
  //             "".to_string()
  //           } else {
  //             f.to_string()
  //           }
  //         })
  //         .collect::<String>();

  //       spider::tokio::fs::create_dir_all(&base_dir).await?;

  //       if !p.contains(".") {
  //         p + ".jsonl"
  //       } else {
  //         p
  //       }
  //     }
  //     _ => {
  //       spider::tokio::fs::create_dir_all("./storage").await?;
  //       "./storage/".to_owned()
  //         + &self
  //           .inner
  //           .get_domain()
  //           .inner()
  //           .replace("http://", "")
  //           .replace("https://", "")
  //         + "jsonl"
  //     }
  //   };
  //   let mut file = spider::tokio::fs::File::create(file).await?;
  //   // transform data step needed to auto convert type ..
  //   file.write_all(&self.collected_data).await?;
  //   Ok(())
  // }

  /// subscribe and add an event listener.
  pub fn subscribe(mut slf: PyRefMut<'_, Self>, on_page_event: PyObject) -> u32 {
    let mut rx2 = slf
      .inner
      .subscribe(*BUFFER / 2)
      .expect("sync feature should be enabled");
    let raw_content = slf.raw_content;

    let handle = pyo3_asyncio::tokio::get_runtime().spawn(async move {
      while let Ok(res) = rx2.recv().await {
        let page = NPage::new(&res, raw_content);
        Python::with_gil(|py| {
          let _ = on_page_event.call(py, (page, 0), None);
        });
      }
    });

    // always return the highest value as the next id.
    let id = match slf.subscription_handles.last() {
      Some(handle) => handle.0 + 1,
      _ => 0,
    };

    slf.subscription_handles.insert(id, handle);

    id
  }

  /// remove a subscription listener.
  pub fn unsubscribe(&mut self, id: Option<u32>) -> bool {
    match id {
      Some(id) => {
        let handle = self.subscription_handles.get(&id);

        match handle {
          Some(h) => {
            h.abort();
            self.subscription_handles.remove_entry(&id);
            true
          }
          _ => false,
        }
      }
      // we may want to get all subs and remove them
      _ => {
        let keys = self.subscription_handles.len();
        for k in self.subscription_handles.drain(..) {
          k.1.abort();
        }
        keys > 0
      }
    }
  }

  /// stop a crawl
  pub fn stop(mut slf: PyRefMut<'_, Self>, id: Option<u32>) -> bool {
    slf.inner.stop();

    // prevent the last background run
    if slf.running_in_background {
      let domain_name = slf.inner.get_domain().inner().clone();

      let _ = pyo3_asyncio::tokio::future_into_py(slf.py(), async move {
        shutdown(&domain_name).await;
        Ok(())
      });

      slf.running_in_background = false;
    }

    match id {
      Some(id) => {
        let handle = slf.crawl_handles.get(&id);

        match handle {
          Some(h) => {
            h.abort();
            slf.crawl_handles.remove_entry(&id);
            true
          }
          _ => false,
        }
      }
      _ => {
        let keys = slf.crawl_handles.len();
        for k in slf.crawl_handles.drain(..) {
          k.1.abort();
        }
        keys > 0
      }
    }
  }

  /// crawl a website
  pub fn crawl(
    mut slf: PyRefMut<'_, Self>,
    on_page_event: Option<PyObject>,
    background: Option<bool>,
    headless: Option<bool>,
  ) {
    // only run in background if on_page_event is handled for streaming.
    let background = background.is_some() && background.unwrap_or_default();
    let headless = headless.is_some() && headless.unwrap_or_default();
    let raw_content = slf.raw_content;

    if background {
      slf.running_in_background = background;
    }

    match on_page_event {
      Some(callback) => {
        if background {
          let mut website = slf.inner.clone();
          let mut rx2 = website
            .subscribe(*BUFFER / 2)
            .expect("sync feature should be enabled");

          let handle = spider::tokio::spawn(async move {
            while let Ok(res) = rx2.recv().await {
              let page = NPage::new(&res, raw_content);
              Python::with_gil(|py| {
                let _ = callback.call(py, (page, 0), None);
              });
            }
          });

          let crawl_id = match slf.crawl_handles.last() {
            Some(handle) => handle.0 + 1,
            _ => 0,
          };

          let crawl_handle = spider::tokio::spawn(async move {
            if headless {
              website.crawl().await;
            } else {
              website.crawl_raw().await;
            }
          });

          let id = match slf.subscription_handles.last() {
            Some(handle) => handle.0 + 1,
            _ => 0,
          };

          slf.crawl_handles.insert(crawl_id, crawl_handle);
          slf.subscription_handles.insert(id, handle);
        } else {
          let mut rx2 = slf
            .inner
            .subscribe(*BUFFER / 2)
            .expect("sync feature should be enabled");

          let handle = pyo3_asyncio::tokio::get_runtime().spawn(async move {
            while let Ok(res) = rx2.recv().await {
              Python::with_gil(|py| {
                let _ = callback.call(py, (NPage::new(&res, raw_content), 0), None);
              });
            }
          });

          let id = match slf.subscription_handles.last() {
            Some(handle) => handle.0 + 1,
            _ => 0,
          };

          slf.subscription_handles.insert(id, handle);

          let _ = pyo3_asyncio::tokio::get_runtime().block_on(async move {
            if headless {
              slf.inner.crawl().await;
            } else {
              slf.inner.crawl_raw().await;
            }
            Ok::<(), ()>(())
          });
        }
      }
      _ => {
        if background {
          let mut website = slf.inner.clone();

          let crawl_id = match slf.crawl_handles.last() {
            Some(handle) => handle.0 + 1,
            _ => 0,
          };

          let crawl_handle = spider::tokio::spawn(async move {
            if headless {
              website.crawl().await;
            } else {
              website.crawl_raw().await;
            }
          });

          slf.crawl_handles.insert(crawl_id, crawl_handle);
        } else {
          let _ = pyo3_asyncio::tokio::get_runtime().block_on(async move {
            if headless {
              slf.inner.crawl().await;
            } else {
              slf.inner.crawl_raw().await;
            }
            Ok::<(), ()>(())
          });
        }
      }
    };
  }

  /// scrape a website
  pub fn scrape(
    mut slf: PyRefMut<'_, Self>,
    on_page_event: Option<PyObject>,
    background: Option<bool>,
    headless: Option<bool>,
  ) {
    let headless = headless.is_some() && headless.unwrap_or_default();
    let raw_content = slf.raw_content;
    let background = background.is_some() && background.unwrap_or_default();

    if background {
      slf.running_in_background = background;
    }

    match on_page_event {
      Some(callback) => {
        if background {
          let mut website = slf.inner.clone();
          let mut rx2 = website
            .subscribe(*BUFFER / 2)
            .expect("sync feature should be enabled");

          let handle = spider::tokio::spawn(async move {
            while let Ok(res) = rx2.recv().await {
              Python::with_gil(|py| {
                let _ = callback.call(py, (NPage::new(&res, raw_content), 0), None);
              });
            }
          });

          let crawl_id = match slf.crawl_handles.last() {
            Some(handle) => handle.0 + 1,
            _ => 0,
          };

          let crawl_handle = spider::tokio::spawn(async move {
            if headless {
              website.scrape().await;
            } else {
              website.scrape_raw().await;
            }
          });

          let id = match slf.subscription_handles.last() {
            Some(handle) => handle.0 + 1,
            _ => 0,
          };

          slf.crawl_handles.insert(crawl_id, crawl_handle);
          slf.subscription_handles.insert(id, handle);
        } else {
          let mut rx2 = slf
            .inner
            .subscribe(*BUFFER / 2)
            .expect("sync feature should be enabled");

          let handle = pyo3_asyncio::tokio::get_runtime().spawn(async move {
            while let Ok(res) = rx2.recv().await {
              Python::with_gil(|py| {
                let _ = callback.call(py, (NPage::new(&res, raw_content), 0), None);
              });
            }
          });

          let id = match slf.subscription_handles.last() {
            Some(handle) => handle.0 + 1,
            _ => 0,
          };

          slf.subscription_handles.insert(id, handle);

          let _ = pyo3_asyncio::tokio::get_runtime().block_on(async move {
            if headless {
              slf.inner.scrape().await;
            } else {
              slf.inner.scrape_raw().await;
            }
            Ok::<(), ()>(())
          });
        }
      }
      _ => {
        if background {
          let mut website = slf.inner.clone();

          let crawl_id = match slf.crawl_handles.last() {
            Some(handle) => handle.0 + 1,
            _ => 0,
          };

          let crawl_handle = spider::tokio::spawn(async move {
            if headless {
              website.scrape().await;
            } else {
              website.scrape_raw().await;
            }
          });

          slf.crawl_handles.insert(crawl_id, crawl_handle);
        } else {
          let _ = pyo3_asyncio::tokio::get_runtime().block_on(async move {
            if headless {
              slf.inner.scrape().await;
            } else {
              slf.inner.scrape_raw().await;
            }
            Ok::<(), ()>(())
          });
        }
      }
    }
  }

  /// run a cron job
  pub fn run_cron(mut slf: PyRefMut<'_, Self>, on_page_event: Option<PyObject>) -> Cron {
    let cron_handle = match on_page_event {
      Some(callback) => {
        let mut rx2 = slf
          .inner
          .subscribe(*BUFFER / 2)
          .expect("sync feature should be enabled");
        let raw_content = slf.raw_content;

        let handler = spider::tokio::spawn(async move {
          while let Ok(res) = rx2.recv().await {
            Python::with_gil(|py| {
              let _ = callback.call(py, (NPage::new(&res, raw_content), 0), None);
            });
          }
        });

        Some(handler)
      }
      _ => None,
    };

    let inner = pyo3_asyncio::tokio::get_runtime()
      .block_on(async move {
        let runner: spider::async_job::Runner = slf.inner.run_cron().await;
        Ok::<spider::async_job::Runner, ()>(runner)
      })
      .unwrap();

    Cron { inner, cron_handle }
  }

  /// get all the links of a website
  pub fn get_links(&self) -> Vec<String> {
    let links = self
      .inner
      .get_links()
      .iter()
      .map(|x| x.as_ref().to_string())
      .collect::<Vec<String>>();
    links
  }

  /// get the size of the website in amount of pages crawled. If you ran the page in the background, this value will not update.
  pub fn size(&mut self) -> u32 {
    self.inner.size() as u32
  }

  /// get the configuration custom HTTP headers
  pub fn get_configuration_headers(&self) -> Vec<(String, String)> {
    let mut map = Vec::new();

    match self.inner.configuration.headers.as_ref() {
      Some(h) => {
        for v in h.iter() {
          let mut value = String::new();

          match v.1.to_str() {
            Ok(vv) => {
              value.push_str(vv);
            }
            _ => (),
          };

          map.push((v.0.to_string(), value))
        }
      }
      _ => (),
    }

    map
  }

  /// get all the pages of a website - requires calling website.scrape
  pub fn get_pages(&self) -> Vec<NPage> {
    let mut pages: Vec<NPage> = Vec::new();
    let raw_content = self.raw_content;

    match self.inner.get_pages() {
      Some(p) => {
        for page in p.iter() {
          pages.push(NPage::new(page, raw_content));
        }
      }
      _ => (),
    }

    pages
  }

  /// drain all links from storing
  pub fn drain_links(&mut self) -> Vec<String> {
    let links = self
      .inner
      .drain_links()
      .map(|x| x.as_ref().to_string())
      .collect::<Vec<String>>();

    links
  }

  /// clear all links and page data
  pub fn clear(&mut self) {
    self.inner.clear();
  }

  /// Set HTTP headers for request using [reqwest::header::HeaderMap](https://docs.rs/reqwest/latest/reqwest/header/struct.HeaderMap.html).
  pub fn with_headers(
    mut slf: PyRefMut<'_, Self>,
    headers: Option<PyObject>,
  ) -> PyRefMut<'_, Self> {
    use pyo3::types::PyDict;
    use std::str::FromStr;
    match headers {
      Some(obj) => {
        let mut h = spider::reqwest::header::HeaderMap::new();
        let py = slf.py();
        let dict = obj.downcast::<pyo3::types::PyDict>(py);

        match dict {
          Ok(keys) => {
            for key in keys.into_iter() {
              let header_key = spider::reqwest::header::HeaderName::from_str(&key.0.to_string());

              match header_key {
                Ok(hn) => {
                  let header_value = key.1.to_string();

                  match spider::reqwest::header::HeaderValue::from_str(&header_value) {
                    Ok(hk) => {
                      h.append(hn, hk);
                    }
                    _ => (),
                  }
                }
                _ => (),
              }
            }
            slf.inner.with_headers(Some(h));
          }
          _ => (),
        }
      }
      _ => {
        slf.inner.with_headers(None);
      }
    };

    slf
  }

  /// Add user agent to request.
  pub fn with_user_agent(
    mut slf: PyRefMut<'_, Self>,
    user_agent: Option<String>,
  ) -> PyRefMut<'_, Self> {
    slf
      .inner
      .configuration
      .with_user_agent(user_agent.as_deref());
    slf
  }

  /// Respect robots.txt file.
  pub fn with_respect_robots_txt(
    mut slf: PyRefMut<'_, Self>,
    respect_robots_txt: bool,
  ) -> PyRefMut<'_, Self> {
    slf
      .inner
      .configuration
      .with_respect_robots_txt(respect_robots_txt);
    slf
  }

  /// Include subdomains detection.
  pub fn with_subdomains(mut slf: PyRefMut<'_, Self>, subdomains: bool) -> PyRefMut<'_, Self> {
    slf.inner.configuration.with_subdomains(subdomains);
    slf
  }

  /// Include tld detection.
  pub fn with_tld(mut slf: PyRefMut<'_, Self>, tld: bool) -> PyRefMut<'_, Self> {
    slf.inner.configuration.with_tld(tld);
    slf
  }

  /// Only use HTTP/2.
  pub fn with_http2_prior_knowledge(
    mut slf: PyRefMut<'_, Self>,
    http2_prior_knowledge: bool,
  ) -> PyRefMut<'_, Self> {
    slf
      .inner
      .configuration
      .with_http2_prior_knowledge(http2_prior_knowledge);
    slf
  }

  /// Max time to wait for request duration to milliseconds.
  pub fn with_request_timeout(
    mut slf: PyRefMut<'_, Self>,
    request_timeout: Option<u32>,
  ) -> PyRefMut<'_, Self> {
    slf
      .inner
      .configuration
      .with_request_timeout(match request_timeout {
        Some(d) => Some(Duration::from_millis(d.into())),
        _ => None,
      });
    slf
  }

  /// add external domains
  pub fn with_external_domains(
    mut slf: PyRefMut<'_, Self>,
    external_domains: Option<Vec<String>>,
  ) -> PyRefMut<'_, Self> {
    slf.inner.with_external_domains(match external_domains {
      Some(ext) => Some(ext.into_iter()),
      _ => None,
    });
    slf
  }

  /// Set the crawling budget
  pub fn with_budget(
    mut slf: PyRefMut<'_, Self>,
    budget: Option<std::collections::HashMap<String, u32>>,
  ) -> PyRefMut<'_, Self> {
    use spider::hashbrown::hash_map::HashMap;

    match budget {
      Some(d) => {
        let v = d
          .iter()
          .map(|e| e.0.to_owned() + "," + &e.1.to_string())
          .collect::<String>();
        let v = v
          .split(",")
          .collect::<Vec<_>>()
          .chunks(2)
          .map(|x| (x[0], x[1].parse::<u32>().unwrap_or_default()))
          .collect::<HashMap<&str, u32>>();

        slf.inner.with_budget(Some(v));
      }
      _ => (),
    }

    slf
  }

  /// Regex black list urls from the crawl
  pub fn with_blacklist_url(
    mut slf: PyRefMut<'_, Self>,
    blacklist_url: Option<Vec<String>>,
  ) -> PyRefMut<'_, Self> {
    slf
      .inner
      .configuration
      .with_blacklist_url(match blacklist_url {
        Some(v) => {
          let mut blacklist: Vec<CompactString> = Vec::new();
          for item in v {
            blacklist.push(CompactString::new(item));
          }
          Some(blacklist)
        }
        _ => None,
      });

    slf
  }

  /// Setup cron jobs to run
  pub fn with_cron(
    mut slf: PyRefMut<'_, Self>,
    cron_str: String,
    cron_type: Option<String>,
  ) -> PyRefMut<'_, Self> {
    slf.inner.with_cron(
      cron_str.as_str(),
      if cron_type.unwrap_or_default() == "scrape" {
        spider::website::CronType::Scrape
      } else {
        spider::website::CronType::Crawl
      },
    );
    slf
  }

  /// Delay between request as ms.
  pub fn with_delay(mut slf: PyRefMut<'_, Self>, delay: u32) -> PyRefMut<'_, Self> {
    slf.inner.configuration.with_delay(delay.into());
    slf
  }

  /// Use proxies for request.
  pub fn with_proxies(
    mut slf: PyRefMut<'_, Self>,
    proxies: Option<Vec<String>>,
  ) -> PyRefMut<'_, Self> {
    slf.inner.configuration.with_proxies(proxies);
    slf
  }

  /// build the inner website - not required for all builder_steps
  pub fn build(mut slf: PyRefMut<'_, Self>) -> PyRefMut<'_, Self> {
    match slf.inner.build() {
      Ok(w) => slf.inner = w,
      _ => (),
    }
    slf
  }
}

/// a runner for handling crons
#[pyclass]
pub struct Cron {
  /// the runner task
  inner: spider::async_job::Runner,
  /// inner cron handle
  cron_handle: Option<JoinHandle<()>>,
}

#[pymethods]
impl Cron {
  /// stop the cron instance
  pub fn stop(mut slf: PyRefMut<'_, Self>) {
    match &slf.cron_handle {
      Some(h) => h.abort(),
      _ => (),
    };
    let _ = pyo3_asyncio::tokio::get_runtime().block_on(async move {
      slf.inner.stop().await;
      Ok::<(), ()>(())
    });
  }
}
