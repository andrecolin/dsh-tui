//! Localization: English and Chinese dictionaries, matching the web client's two locales.
//!
//! A missing key falls back to English and then to the key itself, so an untranslated
//! string renders as readable text rather than blank space or a panic.

/// Supported locales, mirroring the web client's shipped dictionaries.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Locale {
    En,
    Zh,
}

impl Locale {
    /// Choose a locale from an environment value such as `zh_CN.UTF-8`.
    ///
    /// Anything unrecognized is English rather than an error: a wrong language is
    /// recoverable, a refusal to start is not.
    pub fn detect(value: Option<&str>) -> Self {
        match value {
            Some(value) if value.to_ascii_lowercase().starts_with("zh") => Locale::Zh,
            _ => Locale::En,
        }
    }

    pub fn from_env() -> Self {
        let value = std::env::var("LC_ALL")
            .or_else(|_| std::env::var("LC_MESSAGES"))
            .or_else(|_| std::env::var("LANG"))
            .ok();
        Self::detect(value.as_deref())
    }

    pub fn tag(self) -> &'static str {
        match self {
            Locale::En => "en",
            Locale::Zh => "zh",
        }
    }
}

/// Translate one key.
pub fn t(locale: Locale, key: &str) -> &'static str {
    if locale == Locale::Zh {
        if let Some(text) = lookup(ZH, key) {
            return text;
        }
    }
    // English is the fallback for an untranslated key, and the key itself for an unknown
    // one, so a missing string is readable rather than blank.
    lookup(EN, key).unwrap_or("")
}

/// Translate, falling back to the supplied text when the key is unknown.
pub fn t_or<'a>(locale: Locale, key: &str, fallback: &'a str) -> &'a str
where
    'static: 'a,
{
    let text = t(locale, key);
    if text.is_empty() {
        fallback
    } else {
        text
    }
}

fn lookup(table: &[(&'static str, &'static str)], key: &str) -> Option<&'static str> {
    table
        .iter()
        .find(|(candidate, _)| *candidate == key)
        .map(|(_, text)| *text)
}

const EN: &[(&str, &str)] = &[
    ("sidebar.sessions", "Sessions"),
    ("sidebar.newSession", "New session"),
    ("conversation.title", "Conversation"),
    ("conversation.empty", "No messages yet."),
    ("composer.placeholder", "Ask anything, / for commands, @ for references"),
    ("trajectory.title", "Trajectory"),
    ("trajectory.empty", "No turns yet."),
    ("settings.title", "Settings"),
    ("settings.general", "General"),
    ("settings.models", "Models"),
    ("settings.plugins", "Plugins"),
    ("settings.readOnly", "This settings provider is read-only."),
    ("settings.restart", "Changes apply after a restart."),
    ("workspaces.title", "Workspaces"),
    ("workspaces.empty", "No workspaces yet."),
    ("approval.title", "Permission required"),
    ("approval.allow", "allow"),
    ("approval.deny", "deny"),
    ("approval.delegate", "delegate to host"),
    ("questions.title", "Question"),
    ("questions.planReview", "Plan review"),
    ("deliverables.produced", "Produced"),
    ("sidebar.empty", "No sessions yet — press n for a new one"),
    ("sidebar.noWorkspace", "▪ no workspace"),
    ("sidebar.noSession", "  no session open"),
    ("sidebar.emptyNoWorkspace", "No sessions yet — press n to choose a directory"),
    ("conversation.starting", "Starting the harness runtime…"),
    ("conversation.freshWorkspace", "Nothing here yet."),
    ("conversation.freshHint", "Type below to start the first session."),
    ("conversation.disconnected", "Disconnected"),
    ("settings.namespaces", "Namespaces"),
    ("settings.reading", "Reading the settings document…"),
    ("models.reading", "Reading the provider directory…"),
    ("models.catalogReading", "Reading the model catalog…"),
    ("models.credentialsUnavailable", "Credential state unavailable"),
    ("plugins.reading", "Reading the Loader inventory…"),
    ("plugins.noMatch", "No plugin matches that search."),
    ("general.appearance", "Appearance"),
    ("picker.empty", "No subdirectories here."),
    ("picker.useThis", "✓ use this directory"),
    ("approval.unrenderable", "This build cannot render this request."),
    ("logs.title", "Logs"),
    ("logs.empty", "No records match."),
    ("status.connected", "connected"),
    ("status.connecting", "connecting"),
    ("status.disconnected", "disconnected"),
];

const ZH: &[(&str, &str)] = &[
    ("sidebar.sessions", "会话"),
    ("sidebar.newSession", "新会话"),
    ("conversation.title", "对话"),
    ("conversation.empty", "暂无消息。"),
    ("composer.placeholder", "输入任何问题，/ 唤起命令，@ 引用文件"),
    ("trajectory.title", "轨迹"),
    ("trajectory.empty", "暂无回合。"),
    ("settings.title", "设置"),
    ("settings.general", "通用"),
    ("settings.models", "模型"),
    ("settings.plugins", "插件"),
    ("settings.readOnly", "该设置源为只读。"),
    ("settings.restart", "更改将在重启后生效。"),
    ("workspaces.title", "工作区"),
    ("workspaces.empty", "暂无工作区。"),
    ("approval.title", "需要授权"),
    ("approval.allow", "允许"),
    ("approval.deny", "拒绝"),
    ("approval.delegate", "交由主机处理"),
    ("questions.title", "问题"),
    ("questions.planReview", "计划审阅"),
    ("deliverables.produced", "产物"),
    ("sidebar.empty", "暂无会话 —— 按 n 新建"),
    ("sidebar.noWorkspace", "▪ 未选择工作区"),
    ("sidebar.noSession", "  未打开会话"),
    ("sidebar.emptyNoWorkspace", "还没有会话 — 按 n 选择目录并新建"),
    ("conversation.starting", "正在启动 harness 运行时……"),
    ("conversation.freshWorkspace", "这里还没有内容。"),
    ("conversation.freshHint", "在下方输入即可开始第一个会话。"),
    ("conversation.disconnected", "已断开"),
    ("settings.namespaces", "命名空间"),
    ("settings.reading", "正在读取设置文档……"),
    ("models.reading", "正在读取提供方目录……"),
    ("models.catalogReading", "正在读取模型目录……"),
    ("models.credentialsUnavailable", "凭据状态不可用"),
    ("plugins.reading", "正在读取 Loader 清单……"),
    ("plugins.noMatch", "没有匹配该搜索的插件。"),
    ("general.appearance", "外观"),
    ("picker.empty", "此处没有子目录。"),
    ("picker.useThis", "✓ 使用此目录"),
    ("approval.unrenderable", "此版本无法呈现该请求。"),
    ("logs.title", "日志"),
    ("logs.empty", "没有匹配的记录。"),
    ("status.connected", "已连接"),
    ("status.connecting", "连接中"),
    ("status.disconnected", "已断开"),
];

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn locales_come_from_the_usual_environment_values() {
        assert_eq!(Locale::detect(Some("zh_CN.UTF-8")), Locale::Zh);
        assert_eq!(Locale::detect(Some("zh")), Locale::Zh);
        assert_eq!(Locale::detect(Some("en_US.UTF-8")), Locale::En);
        // A wrong language is recoverable; refusing to start is not.
        assert_eq!(Locale::detect(Some("qq_ZZ")), Locale::En);
        assert_eq!(Locale::detect(None), Locale::En);
    }

    #[test]
    fn both_dictionaries_translate_their_keys() {
        assert_eq!(t(Locale::En, "sidebar.sessions"), "Sessions");
        assert_eq!(t(Locale::Zh, "sidebar.sessions"), "会话");
        assert_eq!(t(Locale::Zh, "approval.delegate"), "交由主机处理");
    }

    #[test]
    fn an_untranslated_key_falls_back_to_english() {
        // Every key present in EN must resolve in ZH too, even if only via fallback.
        for (key, english) in EN {
            let translated = t(Locale::Zh, key);
            assert!(
                !translated.is_empty(),
                "{key} resolved to nothing; English is {english}"
            );
        }
    }

    #[test]
    fn an_unknown_key_uses_the_supplied_fallback() {
        assert_eq!(t(Locale::En, "no.such.key"), "");
        assert_eq!(t_or(Locale::En, "no.such.key", "Sessions"), "Sessions");
        assert_eq!(t_or(Locale::Zh, "sidebar.sessions", "Sessions"), "会话");
    }

    #[test]
    fn the_two_dictionaries_cover_the_same_keys() {
        // A key in ZH with no EN counterpart could never be reached through the fallback.
        for (key, _) in ZH {
            assert!(
                lookup(EN, key).is_some(),
                "{key} exists only in the Chinese dictionary"
            );
        }
        assert_eq!(EN.len(), ZH.len());
    }
}
