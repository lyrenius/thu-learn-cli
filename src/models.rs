//! Data models for Tsinghua Learn.
//!
//! Learn API JSON fields use pinyin abbreviations. Parsing stays in
//! [`crate::api`] with `serde_json::Value`; this module exposes clean models.

use serde::{Deserialize, Serialize};

/// Generate a stable seven-character hex ID from a long server ID.
pub fn short_id(full: &str) -> String {
    use std::hash::{Hash, Hasher};
    let mut h = std::collections::hash_map::DefaultHasher::new();
    full.hash(&mut h);
    format!("{:07x}", h.finish() & 0xfff_ffff)
}

/// A course.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[allow(dead_code)] // Reserved for future course display.
pub struct Course {
    /// `wlkcid`: 网络课程 ID / online course ID.
    pub id: String,
    /// Course name.
    pub name: String,
    /// Teacher name.
    pub teacher: String,
}

/// Homework submission status.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub enum HomeworkStatus {
    /// Not submitted yet.
    Pending,
    /// Submitted but not graded.
    Submitted,
    /// Graded.
    Graded,
}

impl HomeworkStatus {
    pub fn label(self) -> &'static str {
        match self {
            HomeworkStatus::Pending => "Pending",
            HomeworkStatus::Submitted => "Submitted",
            HomeworkStatus::Graded => "Graded",
        }
    }
}

/// A homework item.
#[derive(Debug, Clone, Serialize)]
#[allow(dead_code)] // Reserved for future detail and display improvements.
pub struct Homework {
    /// `xszyid`: 学生作业 ID / student homework ID.
    pub student_homework_id: String,
    /// `zyid`: 作业 ID / homework base ID.
    pub base_id: String,
    /// `wlkcid`: 网络课程 ID / online course ID.
    pub course_id: String,
    /// Course name, backfilled by course ID when the list response omits it.
    pub course_name: String,
    /// Homework title.
    pub title: String,
    /// Parsed deadline from a millisecond timestamp or formatted date string.
    pub deadline: Option<chrono::DateTime<chrono::Local>>,
    /// `jzsj`: 截止时间 / deadline, kept as the raw server string.
    pub deadline_raw: String,
    /// `scsj`: 上传时间 / submission time, present for submitted or graded items.
    pub submit_time: String,
    /// `cj` or `djzcj`: 成绩 / grade. `-100` means reviewed without a numeric grade.
    pub grade: Option<String>,
    /// `pynr`: 评语内容 / grading comment, present after grading when available.
    pub comment: String,
    /// `jsm`: 教师名 / grader name.
    pub grader: String,
    /// `pysj` or `pysjStr`: 评阅时间 / grading time.
    pub grade_time: String,
    pub status: HomeworkStatus,
}

/// A course announcement.
#[derive(Debug, Clone, Serialize)]
#[allow(dead_code)] // Reserved for `ann show <id>` and future display improvements.
pub struct Notification {
    /// `ggid`: 公告 ID / announcement ID.
    pub id: String,
    /// `wlkcid`: 网络课程 ID / online course ID.
    pub course_id: String,
    pub course_name: String,
    /// `bt` or `ggbt`: 标题 / title.
    pub title: String,
    /// `fbsj` or `fbsjStr`: 发布时间 / publish time.
    pub publish_time: String,
    /// `fbr` or `fbrxm`: 发布人 / publisher.
    pub publisher: String,
    /// `sfyd`: 是否已读 / whether the announcement has been read.
    pub read: bool,
    /// `ggnr` or `ggnrStr`: 公告内容 / announcement body.
    pub content: String,
}

/// A file attached to a homework page.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct HomeworkAttachment {
    /// Attachment section, normalized from Learn's Chinese section labels.
    pub section: String,
    /// File name.
    pub filename: String,
    /// Download URL, completed with the Learn origin and authenticated with `_csrf`.
    pub download_path: String,
}

/// A course file.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[allow(dead_code)] // Reserved for grouped display.
pub struct CourseFile {
    /// `wjid`: 文件 ID / file ID used for downloads.
    pub id: String,
    /// `wlkcid`: 网络课程 ID / online course ID.
    pub course_id: String,
    pub course_name: String,
    /// `bt` or `wjbt`: 标题 / title.
    pub title: String,
    /// `wjdx` or `fileSize`: 文件大小 / file size, already formatted by Learn.
    pub size: String,
    /// `wjlx`: 文件类型 / file type or extension.
    pub file_type: String,
    /// `scsj`: 上传时间 / upload time.
    pub upload_time: String,
    /// `ms`: 描述 / description, often empty.
    pub description: String,
}

#[cfg(test)]
mod tests {
    use super::HomeworkStatus;

    #[test]
    fn homework_status_labels_are_english() {
        assert_eq!(HomeworkStatus::Pending.label(), "Pending");
        assert_eq!(HomeworkStatus::Submitted.label(), "Submitted");
        assert_eq!(HomeworkStatus::Graded.label(), "Graded");
    }
}
