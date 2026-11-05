//! High-level Tsinghua Learn API methods built on top of [`Client`].
//!
//! Learn returns pinyin-abbreviated JSON fields that can vary by endpoint and
//! version. This module keeps parsing defensive with `serde_json::Value` and
//! ordered candidate keys.

use anyhow::{Context, Result};
use serde_json::Value;

use crate::client::Client;
use crate::models::*;

const LEARN: &str = "https://learn.tsinghua.edu.cn";

impl Client {
    /// Current semester ID, such as `2023-2024-2`, cached for six hours.
    pub async fn current_semester(&self) -> Result<String> {
        crate::cache::with_cache(
            "semester",
            std::time::Duration::from_secs(6 * 3600),
            async {
                let url = format!("{LEARN}/b/kc/zhjw_v_code_xnxq/getCurrentAndNextSemester");
                let v = self.get_json(&url).await?;
                let xnxq = pick(&v["result"], &["xnxq", "id"]);
                if xnxq.is_empty() {
                    anyhow::bail!("Could not parse current semester from response: {v}");
                }
                Ok(xnxq)
            },
        )
        .await
    }

    /// Courses for one semester, cached for six hours.
    pub async fn course_list(&self, semester: &str) -> Result<Vec<Course>> {
        crate::cache::with_cache(
            &format!("courses_{semester}"),
            std::time::Duration::from_secs(6 * 3600),
            async {
                let url = format!(
                    "{LEARN}/b/wlxt/kc/v_wlkc_xs_xkb_kcb_extend/student/loadCourseBySemesterId/{semester}/zh"
                );
                let v = self.get_json(&url).await?;
                let arr = find_array(&v, &["resultList", "object", "aaData"])
                    .context("Course list response did not contain an array")?;
                let courses = arr
                    .iter()
                    .map(|c| Course {
                        // wlkcid = wangluo kecheng id = network course ID.
                        id: pick(c, &["wlkcid", "wlkc_id", "kcid"]),
                        // kcm/kcmc/zwkcm = kecheng ming/cheng = course name.
                        name: decode(&pick(c, &["kcm", "kcmc", "zwkcm"])),
                        // jsm/js/jsmc/skjs = jiaoshi ming = teacher name.
                        teacher: decode(&pick(c, &["jsm", "js", "jsmc", "skjs"])),
                    })
                    .filter(|c| !c.id.is_empty())
                    .collect();
                Ok(courses)
            },
        )
        .await
    }

    /// All homework merged across the three status endpoints.
    pub async fn homework_list(&self, courses: &[Course]) -> Result<Vec<Homework>> {
        let sources = [
            (
                format!("{LEARN}/b/wlxt/kczy/zy/student/zyListWj"),
                HomeworkStatus::Pending,
            ),
            (
                format!("{LEARN}/b/wlxt/kczy/zy/student/zyListYjwg"),
                HomeworkStatus::Submitted,
            ),
            (
                format!("{LEARN}/b/wlxt/kczy/zy/student/zyListYpg"),
                HomeworkStatus::Graded,
            ),
        ];

        // Query all three status endpoints concurrently; `aoData=[]` means no course filter.
        let futs = sources.iter().map(|(url, status)| async move {
            let form = || reqwest::multipart::Form::new().text("aoData", "[]");
            let v = match self.post_multipart_json(url, form).await {
                Ok(v) => v,
                Err(e) => return auth_error_or_empty(e),
            };
            let Some(arr) = find_array(&v, &["aaData", "object", "resultList"]) else {
                return Ok(Vec::new());
            };
            Ok(arr
                .iter()
                .map(|h| {
                    // wlkcid = wangluo kecheng id = network course ID.
                    let course_id = pick(h, &["wlkcid", "wlkc_id", "kcid"]);
                    let course_name = {
                        // kcm/wlkcm/kcmc = kecheng ming = course name.
                        let n = decode(&pick(h, &["kcm", "wlkcm", "kcmc"]));
                        if n.is_empty() {
                            course_name_of(courses, &course_id)
                        } else {
                            n
                        }
                    };
                    // jzsj = jiezhishijian = deadline time.
                    let deadline_raw = pick(h, &["jzsj"]);
                    Homework {
                        // xszyid = xuesheng zuoye id = student homework ID; zyid = zuoye id = homework ID.
                        student_homework_id: pick(h, &["xszyid", "zyid"]),
                        base_id: pick(h, &["zyid"]),
                        course_id,
                        course_name,
                        // bt/zybt = biaoti/zuoye biaoti = title/homework title.
                        title: decode(&pick(h, &["bt", "zybt"])),
                        deadline: parse_deadline(&deadline_raw),
                        deadline_raw,
                        // scsj = shangchuanshijian = upload/submission time.
                        submit_time: pick(h, &["scsj"]),
                        grade: parse_grade(h),
                        // pynr = pingyu neirong = grading comment.
                        comment: strip_html(&pick(h, &["pynr"])),
                        // jsm = jiaoshi ming = teacher name; pysj = pingyueshijian = grading time.
                        grader: decode(&pick(h, &["jsm"])),
                        grade_time: pick(h, &["pysjStr", "pysj"]),
                        status: *status,
                    }
                })
                .collect::<Vec<_>>())
        });
        let nested = futures::future::join_all(futs).await;
        flatten_results(nested)
    }

    /// Homework description. The `id` form field is the base homework ID (`zyid`), and `msg` carries the body.
    pub async fn homework_detail(&self, zyid: &str) -> Result<String> {
        let url = format!("{LEARN}/b/wlxt/kczy/zy/student/detail");
        let form = || reqwest::multipart::Form::new().text("id", zyid.to_string());
        let v = self.post_multipart_json(&url, form).await?;
        Ok(strip_html(&pick(&v, &["msg"])))
    }

    /// Homework attachments parsed from the `viewCj` HTML page.
    pub async fn homework_attachments(
        &self,
        wlkcid: &str,
        xszyid: &str,
    ) -> Result<Vec<HomeworkAttachment>> {
        let page = format!("{LEARN}/f/wlxt/kczy/zy/student/viewCj?wlkcid={wlkcid}&xszyid={xszyid}");
        let html = self.get_text(&page).await?;
        Ok(parse_homework_attachments(&html))
    }

    /// Non-expired course announcements, fetched per course because empty `aoData` is rejected.
    pub async fn notification_list(&self, courses: &[Course]) -> Result<Vec<Notification>> {
        let url = format!("{LEARN}/b/wlxt/kcgg/wlkc_ggb/student/pageListXsbyWgq");
        // Fetch each course's announcements concurrently.
        let futs = courses.iter().map(|course| {
            let url = &url;
            async move {
                let aodata = format!("[{{\"name\":\"wlkcid\",\"value\":\"{}\"}}]", course.id);
                let form = || reqwest::multipart::Form::new().text("aoData", aodata.clone());
                let v = match self.post_multipart_json(url, form).await {
                    Ok(v) => v,
                    Err(e) => return auth_error_or_empty(e),
                };
                let Some(arr) = find_array(&v, &["aaData", "object", "resultList"]) else {
                    return Ok(Vec::new());
                };
                Ok(arr
                    .iter()
                    .map(|n| Notification {
                        // ggid = gonggao id = announcement ID.
                        id: pick(n, &["ggid", "id"]),
                        course_id: course.id.clone(),
                        course_name: course.name.clone(),
                        // ggbt = gonggao biaoti = announcement title.
                        title: decode(&pick(n, &["bt", "ggbt"])),
                        // fbsj = fabushijian = publish time; fbr = faburen = publisher.
                        publish_time: pick(n, &["fbsjStr", "fbsj"]),
                        publisher: decode(&pick(n, &["fbrxm", "fbr"])),
                        // sfyd = shifou yidu = whether read. Learn returns the Chinese literal `是` for yes.
                        read: pick(n, &["sfyd"]) == "是",
                        // ggnr = gonggao neirong = announcement content.
                        content: strip_html(&pick(n, &["ggnrStr", "ggnr"])),
                    })
                    .collect::<Vec<_>>())
            }
        });
        let nested = futures::future::join_all(futs).await;
        flatten_results(nested)
    }

    /// Course file list for one course.
    pub async fn file_list(&self, course: &Course) -> Result<Vec<CourseFile>> {
        let url = format!(
            "{LEARN}/b/wlxt/kj/wlkc_kjxxb/student/kjxxbByWlkcidAndSizeForStudent?wlkcid={}&size=200",
            course.id
        );
        let v = self.get_json(&url).await?;
        let arr = find_array(&v, &["object", "resultList", "aaData"]).unwrap_or_default();
        let out = arr
            .iter()
            .map(|f| CourseFile {
                // wjid = wenjian id = file ID.
                id: pick(f, &["wjid", "fileId"]),
                course_id: course.id.clone(),
                course_name: course.name.clone(),
                // wjbt = wenjian biaoti = file title.
                title: decode(&pick(f, &["bt", "wjbt", "title"])),
                // wjdx = wenjian daxiao = file size; wjlx = wenjian leixing = file type.
                size: pick(f, &["fileSize", "wjdxStr", "wjdx"]),
                file_type: pick(f, &["wjlx", "fileType"]),
                // scsj = shangchuanshijian = upload time; ms = miaoshu = description.
                upload_time: pick(f, &["scsj", "uploadTime"]),
                description: strip_html(&pick(f, &["ms", "wjms"])),
            })
            .filter(|f| !f.id.is_empty())
            .collect();
        Ok(out)
    }

    /// File download URL.
    pub fn file_download_url(&self, file_id: &str) -> String {
        format!("{LEARN}/b/wlxt/kj/wlkc_kjxxb/student/downloadFile?sfgk=0&wjid={file_id}")
    }

    /// Submits homework with a file and optional text comment.
    pub async fn submit_homework(
        &self,
        student_homework_id: &str,
        file: &std::path::Path,
        comment: &str,
    ) -> Result<()> {
        let url = format!("{LEARN}/b/wlxt/kczy/zy/student/tjzy");
        let filename = file
            .file_name()
            .map(|s| s.to_string_lossy().into_owned())
            .unwrap_or_else(|| "submission".into());
        let bytes = tokio::fs::read(file).await?;
        // Rebuilt if the upload is retried after a silent re-login.
        let form = || {
            let part = reqwest::multipart::Part::bytes(bytes.clone()).file_name(filename.clone());
            reqwest::multipart::Form::new()
                .text("xszyid", student_homework_id.to_string())
                .text("isDeleted", "0")
                .text("zynr", comment.to_string())
                .part("fileupload", part)
        };

        let v = self.post_multipart_json(&url, form).await?;
        // Success is returned as `result=success` or a `msg` containing the Learn literal `成功`.
        let result = pick(&v, &["result"]);
        let msg = pick(&v, &["msg", "message"]);
        if result.eq_ignore_ascii_case("success") || msg.contains("成功") {
            Ok(())
        } else {
            anyhow::bail!("Submission may have failed; server returned: {v}");
        }
    }

    /// Prints raw JSON from selected endpoints for field-name investigation.
    pub async fn debug_dump(&self, courses: &[Course]) -> Result<()> {
        let sem = self.current_semester().await.unwrap_or_default();
        println!("=== semester ===\n{sem}\n");

        let course_url = format!(
            "{LEARN}/b/wlxt/kc/v_wlkc_xs_xkb_kcb_extend/student/loadCourseBySemesterId/{sem}/zh"
        );
        dump("course_list", self.get_json(&course_url).await);

        let hw_url = format!("{LEARN}/b/wlxt/kczy/zy/student/zyListWj");
        let form = || reqwest::multipart::Form::new().text("aoData", "[]");
        let hw = self.post_multipart_json(&hw_url, form).await;
        dump("homework_zyListWj(aoData=[])", clone_result(&hw));

        // Probe detail endpoints with the first homework item because they need a real `zyid`.
        if let Ok(v) = &hw {
            if let Some(arr) = find_array(v, &["aaData", "object"]) {
                if let Some(h0) = arr.first() {
                    let zyid = pick(h0, &["zyid"]);
                    let detail = format!("{LEARN}/b/wlxt/kczy/zy/student/detail");
                    let form = || reqwest::multipart::Form::new().text("id", zyid.clone());
                    dump(
                        "homework_detail(POST id=zyid)",
                        self.post_multipart_json(&detail, form).await,
                    );
                }
            }
        }

        let ann_url = format!("{LEARN}/b/wlxt/kcgg/wlkc_ggb/student/pageListXsbyWgq");
        let form = || reqwest::multipart::Form::new().text("aoData", "[]");
        dump(
            "notification_Wgq(aoData=[])",
            self.post_multipart_json(&ann_url, form).await,
        );

        if let Some(c) = courses.first() {
            let aodata = format!("[{{\"name\":\"wlkcid\",\"value\":\"{}\"}}]", c.id);
            let form = || reqwest::multipart::Form::new().text("aoData", aodata.clone());
            dump(
                &format!("notification_Wgq(wlkcid={})", c.id),
                self.post_multipart_json(&ann_url, form).await,
            );

            let file_url = format!(
                "{LEARN}/b/wlxt/kj/wlkc_kjxxb/student/kjxxbByWlkcidAndSizeForStudent?wlkcid={}&size=200",
                c.id
            );
            dump(
                &format!("file_list(wlkcid={})", c.id),
                self.get_json(&file_url).await,
            );
        }
        Ok(())
    }
}

/// Pretty-prints raw JSON, truncated to around 3500 characters for sharing.
fn dump(label: &str, r: Result<serde_json::Value>) {
    match r {
        Ok(v) => {
            let s = serde_json::to_string_pretty(&v).unwrap_or_default();
            let truncated: String = s.chars().take(3500).collect();
            let more = if s.chars().count() > 3500 {
                "\n...(truncated)"
            } else {
                ""
            };
            println!("=== {label} ===\n{truncated}{more}\n");
        }
        Err(e) => println!("=== {label} === ERROR: {e:#}\n"),
    }
}

fn clone_result(r: &Result<serde_json::Value>) -> Result<serde_json::Value> {
    match r {
        Ok(v) => Ok(v.clone()),
        Err(e) => Err(anyhow::anyhow!("{e:#}")),
    }
}

// ---------- Error helpers ----------

/// Keeps the best-effort behavior of list endpoints (a failed course or status
/// list is skipped) but surfaces authentication failures instead of hiding them
/// behind an empty list.
fn auth_error_or_empty<T>(e: anyhow::Error) -> Result<Vec<T>> {
    if e.downcast_ref::<crate::client::AuthError>().is_some() {
        Err(e)
    } else {
        Ok(Vec::new())
    }
}

fn flatten_results<T>(nested: Vec<Result<Vec<T>>>) -> Result<Vec<T>> {
    let mut out = Vec::new();
    for items in nested {
        out.extend(items?);
    }
    Ok(out)
}

// ---------- JSON extraction helpers ----------

/// Picks the first present candidate key and converts strings, numbers, and booleans to text.
fn pick(v: &Value, keys: &[&str]) -> String {
    for k in keys {
        match &v[*k] {
            Value::String(s) => return s.clone(),
            Value::Number(n) => return n.to_string(),
            Value::Bool(b) => return b.to_string(),
            _ => {}
        }
    }
    String::new()
}

/// Finds the first array under any candidate key, recursing into nested objects.
fn find_array<'a>(v: &'a Value, keys: &[&str]) -> Option<Vec<&'a Value>> {
    // Some Learn endpoints return the array as the response root.
    if let Value::Array(a) = v {
        return Some(a.iter().collect());
    }
    for k in keys {
        match &v[*k] {
            Value::Array(a) => return Some(a.iter().collect()),
            nested @ Value::Object(_) => {
                if let Some(a) = find_array(nested, keys) {
                    return Some(a);
                }
            }
            _ => {}
        }
    }
    None
}

/// Parses homework grades. `djzcj` is the level grade, `cj` is the numeric grade, and `cj == -100` means reviewed.
fn parse_grade(h: &Value) -> Option<String> {
    // djzcj = dengjizhi chengji = level grade; cj = chengji = grade.
    let dj = decode(&pick(h, &["djzcj", "ywdjzcj"]));
    if !dj.is_empty() && dj != "0" && dj != "-100" {
        return Some(dj);
    }
    let cj = pick(h, &["cj"]);
    match cj.as_str() {
        "" => None,
        "-100" => Some("Reviewed".to_string()),
        _ => Some(cj),
    }
}

fn course_name_of(courses: &[Course], id: &str) -> String {
    courses
        .iter()
        .find(|c| c.id == id)
        .map(|c| c.name.clone())
        .unwrap_or_default()
}

/// Parses attachments from the homework HTML page.
///
/// Learn section labels are Chinese protocol literals: `作业附件` (homework
/// attachments), `答案附件` (answer attachments), `上交作业附件` (submitted
/// homework attachments), and `评语附件` (comment attachments).
fn parse_homework_attachments(html: &str) -> Vec<HomeworkAttachment> {
    let label_re = regex::Regex::new(
        r#"<div class="left fl">(作业附件|答案附件|上交作业附件|评语附件)</div>"#,
    )
    .unwrap();
    let labels: Vec<(usize, String)> = label_re
        .captures_iter(html)
        .map(|c| (c.get(0).unwrap().start(), c[1].to_string()))
        .collect();

    let a_re =
        regex::Regex::new(r#"(?s)<span class="ftitle">\s*<a\s[^>]*href="([^"]+)"[^>]*>(.*?)</a>"#)
            .unwrap();

    let mut out = Vec::new();
    for cap in a_re.captures_iter(html) {
        let pos = cap.get(0).unwrap().start();
        let href = decode(&cap[1]);
        let filename = strip_html(&cap[2]);
        if filename.is_empty() {
            continue;
        }
        // The section is the closest preceding Learn label before the anchor.
        let section = labels
            .iter()
            .rev()
            .find(|(o, _)| *o < pos)
            .map(|(_, l)| l.clone())
            .unwrap_or_else(|| "Attachment".into());
        // `downloadUrl=` carries the actual file download path.
        let path = href
            .split("downloadUrl=")
            .nth(1)
            .map(str::to_string)
            .unwrap_or_else(|| href.clone());
        let download_path = if path.starts_with("http") {
            path
        } else {
            format!("{LEARN}{path}")
        };
        out.push(HomeworkAttachment {
            section,
            filename,
            download_path,
        });
    }
    out
}

/// Removes tags and decodes entities to make announcement or homework HTML readable.
/// `ggnrStr` contains entity-escaped HTML, for example `&lt;p&gt;Hello&lt;/p&gt;`.
fn strip_html(s: &str) -> String {
    let unescaped = decode(s);
    let mut out = String::new();
    let mut chars = unescaped.chars().peekable();
    let mut link: Option<LinkBuffer> = None;

    while let Some(c) = chars.next() {
        if c == '<' {
            let mut tag = String::new();
            let mut closed = false;
            for t in chars.by_ref() {
                if t == '>' {
                    closed = true;
                    break;
                }
                tag.push(t);
            }
            if closed {
                apply_html_tag(&tag, &mut out, &mut link);
            } else {
                push_html_text(&mut out, &mut link, "<");
                push_html_text(&mut out, &mut link, &tag);
            }
        } else {
            push_html_char(&mut out, &mut link, c);
        }
    }

    if let Some(link) = link.take() {
        push_rendered_link(&mut out, link);
    }

    normalize_rendered_html_text(&decode(&out))
}

struct LinkBuffer {
    href: String,
    text: String,
}

fn apply_html_tag(raw: &str, out: &mut String, link: &mut Option<LinkBuffer>) {
    let tag = raw.trim();
    if tag.is_empty() || tag.starts_with('!') {
        return;
    }

    let closing = tag.starts_with('/');
    let body = tag.trim_start_matches('/').trim();
    let name_end = body
        .find(|c: char| c.is_whitespace() || c == '/')
        .unwrap_or(body.len());
    let name = body[..name_end].to_ascii_lowercase();
    let attrs = body[name_end..].trim();

    if closing {
        if name == "a" {
            if let Some(link) = link.take() {
                push_rendered_link(out, link);
            }
        } else if matches!(name.as_str(), "td" | "th") {
            push_html_text(out, link, " ");
        } else if name == "tr" {
            push_html_break(out, link, 1);
        } else if is_block_html_tag(&name) {
            push_html_break(out, link, 2);
        }
        return;
    }

    match name.as_str() {
        "br" => push_html_break(out, link, 1),
        "hr" => push_html_break(out, link, 2),
        "li" => {
            push_html_break(out, link, 1);
            push_html_text(out, link, "- ");
        }
        "tr" => push_line_start(out, link),
        "td" | "th" => push_html_text(out, link, " "),
        "a" => {
            if let Some(current) = link.take() {
                push_rendered_link(out, current);
            }
            if let Some(href) = html_attr(attrs, "href") {
                *link = Some(LinkBuffer {
                    href,
                    text: String::new(),
                });
            }
        }
        _ if is_block_html_tag(&name) => push_html_break(out, link, 2),
        _ => {}
    }
}

fn push_rendered_link(out: &mut String, link: LinkBuffer) {
    let text = normalize_inline_text(&link.text);
    let href = link.href.trim();
    if href.is_empty() {
        out.push_str(&text);
    } else if text.is_empty() || text == href {
        out.push_str(href);
    } else {
        out.push_str(&format!("{text} ({href})"));
    }
}

fn push_html_char(out: &mut String, link: &mut Option<LinkBuffer>, c: char) {
    if let Some(link) = link {
        link.text.push(c);
    } else {
        out.push(c);
    }
}

fn push_html_text(out: &mut String, link: &mut Option<LinkBuffer>, text: &str) {
    if let Some(link) = link {
        link.text.push_str(text);
    } else {
        out.push_str(text);
    }
}

fn push_html_break(out: &mut String, link: &mut Option<LinkBuffer>, count: usize) {
    for _ in 0..count {
        push_html_char(out, link, '\n');
    }
}

fn push_line_start(out: &mut String, link: &mut Option<LinkBuffer>) {
    let target = link
        .as_ref()
        .map(|link| link.text.as_str())
        .unwrap_or(out.as_str());
    if !target.is_empty() && !target.ends_with('\n') {
        push_html_char(out, link, '\n');
    }
}

fn is_block_html_tag(name: &str) -> bool {
    matches!(
        name,
        "address"
            | "article"
            | "aside"
            | "blockquote"
            | "div"
            | "dl"
            | "fieldset"
            | "figcaption"
            | "figure"
            | "footer"
            | "form"
            | "h1"
            | "h2"
            | "h3"
            | "h4"
            | "h5"
            | "h6"
            | "header"
            | "main"
            | "ol"
            | "p"
            | "pre"
            | "section"
            | "table"
            | "tbody"
            | "tfoot"
            | "thead"
            | "ul"
    )
}

fn html_attr(attrs: &str, name: &str) -> Option<String> {
    let bytes = attrs.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        while i < bytes.len() && bytes[i].is_ascii_whitespace() {
            i += 1;
        }
        let key_start = i;
        while i < bytes.len() && is_attr_name_byte(bytes[i]) {
            i += 1;
        }
        if key_start == i {
            i += 1;
            continue;
        }
        let key = &attrs[key_start..i];
        while i < bytes.len() && bytes[i].is_ascii_whitespace() {
            i += 1;
        }
        if i >= bytes.len() || bytes[i] != b'=' {
            continue;
        }
        i += 1;
        while i < bytes.len() && bytes[i].is_ascii_whitespace() {
            i += 1;
        }
        let value = if i < bytes.len() && (bytes[i] == b'"' || bytes[i] == b'\'') {
            let quote = bytes[i];
            i += 1;
            let value_start = i;
            while i < bytes.len() && bytes[i] != quote {
                i += 1;
            }
            let value = &attrs[value_start..i];
            if i < bytes.len() {
                i += 1;
            }
            value
        } else {
            let value_start = i;
            while i < bytes.len() && !bytes[i].is_ascii_whitespace() {
                i += 1;
            }
            &attrs[value_start..i]
        };
        if key.eq_ignore_ascii_case(name) {
            return Some(decode(value));
        }
    }
    None
}

fn is_attr_name_byte(b: u8) -> bool {
    b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_' | b':')
}

fn normalize_rendered_html_text(s: &str) -> String {
    let mut lines = Vec::new();
    for line in s.lines() {
        let line = normalize_inline_text(line);
        if line.is_empty() {
            if !lines.last().is_none_or(String::is_empty) {
                lines.push(String::new());
            }
        } else {
            lines.push(line);
        }
    }
    while lines.first().is_some_and(String::is_empty) {
        lines.remove(0);
    }
    while lines.last().is_some_and(String::is_empty) {
        lines.pop();
    }
    lines.join("\n")
}

fn normalize_inline_text(s: &str) -> String {
    s.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// Decodes the small set of HTML entities seen in Learn responses.
fn decode(s: &str) -> String {
    s.replace("&lt;", "<")
        .replace("&gt;", ">")
        .replace("&quot;", "\"")
        .replace("&#39;", "'")
        .replace("&nbsp;", " ")
        .replace("&ldquo;", "“")
        .replace("&rdquo;", "”")
        .replace("&lsquo;", "‘")
        .replace("&rsquo;", "’")
        .replace("&mdash;", "—")
        .replace("&hellip;", "…")
        .replace("&middot;", "·")
        // Decode &amp; last so strings such as &amp;lt; do not get double-decoded.
        .replace("&amp;", "&")
        .trim()
        .to_string()
}

/// Parses Learn deadline fields as local time from millisecond timestamps or common date strings.
fn parse_deadline(raw: &str) -> Option<chrono::DateTime<chrono::Local>> {
    use chrono::{Local, NaiveDateTime, TimeZone};
    let raw = raw.trim();
    if raw.is_empty() {
        return None;
    }
    // Millisecond timestamp.
    if let Ok(ms) = raw.parse::<i64>() {
        if ms > 1_000_000_000_000 {
            return Local.timestamp_millis_opt(ms).single();
        }
    }
    for fmt in ["%Y-%m-%d %H:%M:%S", "%Y-%m-%d %H:%M", "%Y-%m-%d"] {
        if let Ok(ndt) = NaiveDateTime::parse_from_str(raw, fmt) {
            return Local.from_local_datetime(&ndt).single();
        }
        // Date-only values are due at the end of that day.
        if let Ok(d) = chrono::NaiveDate::parse_from_str(raw, fmt) {
            let ndt = d.and_hms_opt(23, 59, 0)?;
            return Local.from_local_datetime(&ndt).single();
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_deadline_ms_timestamp() {
        let d = parse_deadline("1781568000000").unwrap();
        assert_eq!(d.timestamp_millis(), 1781568000000);
    }

    #[test]
    fn parse_deadline_datetime_string() {
        let d = parse_deadline("2026-06-16 08:00").unwrap();
        assert_eq!(d.format("%Y-%m-%d %H:%M").to_string(), "2026-06-16 08:00");
    }

    #[test]
    fn parse_deadline_date_only_gets_2359() {
        let d = parse_deadline("2026-06-16").unwrap();
        assert_eq!(d.format("%Y-%m-%d %H:%M").to_string(), "2026-06-16 23:59");
    }

    #[test]
    fn parse_deadline_invalid() {
        assert!(parse_deadline("").is_none());
        assert!(parse_deadline("not a date").is_none());
    }

    #[test]
    fn strip_html_basic() {
        assert_eq!(strip_html("&lt;p&gt;同学们好&lt;/p&gt;"), "同学们好");
    }

    #[test]
    fn strip_html_curly_quotes_and_whitespace() {
        assert_eq!(
            strip_html("&lt;p&gt;a&lt;/p&gt;  &lt;p&gt;b&lt;/p&gt;"),
            "a\n\nb"
        );
        let q = strip_html("&ldquo;OJ&rdquo;");
        assert!(q.contains('“') && q.contains('”'));
    }

    #[test]
    fn strip_html_preserves_announcement_structure_and_links() {
        let html = concat!(
            "&lt;p&gt;各位同学好，本课程计划分组开展Project性能优化。&lt;/p&gt;",
            "&lt;p&gt;每个仓库内包含介绍基本信息的Readme和原始代码code。&lt;/p&gt;",
            "&lt;p&gt;课题一——数值相对论求解优化：",
            "&lt;a href=&quot;https://git.tsinghua.edu.cn/shuixr25/amssncku&quot;&gt;",
            "https://git.tsinghua.edu.cn/shuixr25/amssncku",
            "&lt;/a&gt;&lt;br&gt;",
            "课题二——世界模型推理优化：",
            "&lt;a href=&quot;https://git.tsinghua.edu.cn/shuixr25/unifolm&quot;&gt;",
            "unifolm",
            "&lt;/a&gt;&lt;/p&gt;",
        );

        let text = strip_html(html);

        assert!(text.contains("各位同学好，本课程计划分组开展Project性能优化。"));
        assert!(text.contains("Readme和原始代码code。"));
        assert!(text
            .contains("课题一——数值相对论求解优化：https://git.tsinghua.edu.cn/shuixr25/amssncku"));
        assert!(text.contains(
            "课题二——世界模型推理优化：unifolm (https://git.tsinghua.edu.cn/shuixr25/unifolm)"
        ));
        assert!(text.contains("\n\n每个仓库内"));
        assert!(text.contains("\n课题二"));
        assert!(!text.contains("<a"));
        assert!(!text.contains("\\。"));
    }

    #[test]
    fn strip_html_formats_homework_detail_lists_and_links() {
        let html = concat!(
            "&lt;p&gt;请完成以下任务：&lt;/p&gt;",
            "&lt;ul&gt;",
            "&lt;li&gt;阅读README&lt;/li&gt;",
            "&lt;li&gt;提交报告&lt;/li&gt;",
            "&lt;/ul&gt;",
            "&lt;p&gt;参考链接：",
            "&lt;a href=&quot;https://learn.tsinghua.edu.cn/ref&quot;&gt;说明文档&lt;/a&gt;",
            "&lt;/p&gt;",
        );

        let text = strip_html(html);

        assert_eq!(
            text,
            "请完成以下任务：\n\n- 阅读README\n- 提交报告\n\n参考链接：说明文档 (https://learn.tsinghua.edu.cn/ref)"
        );
    }

    #[test]
    fn strip_html_formats_file_description_tables() {
        let html = concat!(
            "&lt;p&gt;资料说明&lt;/p&gt;",
            "&lt;table&gt;",
            "&lt;tr&gt;&lt;th&gt;文件&lt;/th&gt;&lt;th&gt;用途&lt;/th&gt;&lt;/tr&gt;",
            "&lt;tr&gt;&lt;td&gt;slides.pdf&lt;/td&gt;&lt;td&gt;课件&lt;/td&gt;&lt;/tr&gt;",
            "&lt;/table&gt;",
            "&lt;p&gt;下载镜像：",
            "&lt;a href=&quot;https://learn.tsinghua.edu.cn/files/slides.pdf&quot;&gt;slides.pdf&lt;/a&gt;",
            "&lt;/p&gt;",
        );

        let text = strip_html(html);

        assert_eq!(
            text,
            "资料说明\n\n文件 用途\nslides.pdf 课件\n\n下载镜像：slides.pdf (https://learn.tsinghua.edu.cn/files/slides.pdf)"
        );
    }

    #[test]
    fn decode_entities_and_amp_order() {
        assert_eq!(decode("a&amp;b"), "a&b");
        // &amp; must be decoded last, or &amp;lt; would be decoded twice into <.
        assert_eq!(decode("&amp;lt;"), "&lt;");
        assert_eq!(decode("x&nbsp;y"), "x y");
    }

    #[test]
    fn parse_attachments_extracts_section_name_url() {
        let html = r#"
            <div class="left fl">作业附件</div>
            <span class="ftitle">
            <a target="_blank" href="/f/wlxt/kc/wj_wjb/student/openNewWindow?fileId=ABC&roleType=student&sfgk=0&downloadUrl=/b/wlxt/kczy/zy/student/downloadFile/2025-2026-2151368579/ABC">16.pdf</a>
            </span>
            <div class="left fl">答案附件</div>
            <div class="left fl">评语附件</div>
        "#;
        let atts = parse_homework_attachments(html);
        assert_eq!(atts.len(), 1);
        assert_eq!(atts[0].section, "作业附件");
        assert_eq!(atts[0].filename, "16.pdf");
        assert!(atts[0]
            .download_path
            .ends_with("/b/wlxt/kczy/zy/student/downloadFile/2025-2026-2151368579/ABC"));
        assert!(atts[0]
            .download_path
            .starts_with("https://learn.tsinghua.edu.cn"));
    }

    #[test]
    fn find_array_and_pick() {
        let v = serde_json::json!({"object": {"aaData": [1, 2, 3]}});
        assert_eq!(find_array(&v, &["aaData", "object"]).unwrap().len(), 3);
        let arr = serde_json::json!([1, 2]);
        assert_eq!(find_array(&arr, &["x"]).unwrap().len(), 2);

        let o = serde_json::json!({"a": "hi", "n": 42, "b": true});
        assert_eq!(pick(&o, &["x", "a"]), "hi");
        assert_eq!(pick(&o, &["n"]), "42");
        assert_eq!(pick(&o, &["missing"]), "");
    }

    #[test]
    fn parse_grade_reviewed_sentinel_is_english() {
        let h = serde_json::json!({"cj": -100});
        assert_eq!(parse_grade(&h).as_deref(), Some("Reviewed"));
    }
}
