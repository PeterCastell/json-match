#![deny(unused_must_use)]

use std::{collections::HashSet, sync::LazyLock};

use fancy_regex::Regex;

pub struct Field<'a> {
    pub name: &'a str,
    pub r#type: JsonType<'a>,
    pub predicate: Option<&'a str>,
    pub capture: Option<&'a str>
}
pub enum JsonType<'a> {
    String,
    Number,
    Bool,
    Object,
    Array,
    Literal(&'a str),
    ObjectMatch(ObjectMatch<'a>)
}
pub type ObjectMatch<'a> = &'a [Field<'a>];

impl JsonType<'_> {
    fn open_char(&self) -> Option<&'static str> {
        return match self {
            JsonType::String => Some("\""),
            JsonType::Array => Some("["),
            JsonType::Object | JsonType::ObjectMatch(_) => Some("\\{"),
            _ => None
        };
    }
    fn close_char(&self) -> Option<&'static str> {
        return match self {
            JsonType::String => Some("\""),
            JsonType::Array => Some("]"),
            JsonType::Object | JsonType::ObjectMatch(_) => Some("\\}"),
            _ => None
        };
    }

    fn routine_name(&self) -> char {
        return match self {
            JsonType::String => 's',
            JsonType::Bool => 'b',
            JsonType::Number => 'n',
            JsonType::Array => 'a',
            JsonType::Object => 'o',
            _ => unreachable!()
        };
    }
}

#[derive(Debug, thiserror::Error)]
pub enum CompileError {
    #[error(transparent)]
    Io(#[from] std::fmt::Error),
    #[error(transparent)]
    InvalidPredicate(fancy_regex::Error),
    #[error("No two fields in the same ObjectMatch may share a name")]
    DuplicateFieldName,
    #[error("No two captures may share a name")]
    DuplicateCaptureName,
    #[error("No two captures may share a name")]
    InvalidCaptureName
}

pub fn create_regex_string(root: ObjectMatch) -> Result<String, CompileError> {
    let mut string = String::new();
    create_regex_pattern(root, &mut string)?;
    return Ok(string);
}
static WORD: LazyLock<Regex> = LazyLock::new(|| Regex::new("^\\w+$").expect("This cannot happen"));

pub fn create_regex_pattern(root: ObjectMatch, writer: &mut impl std::fmt::Write) -> Result<(), CompileError> {
    writer.write_str(r#"(?(DEFINE)(?<s>([^"\\]|\\.)*?)(?<n>-?\d+(?:\.\d+)?)(?<b>true|false)(?<a>(\g<v>(,\g<v>)*)?)(?<o>((\g<f>)(,\g<f>)*)?)(?<v>\{\g<o>\}|\[\g<a>\]|"\g<s>"|\g<n>|\g<b>)(?<f>"\g<s>":\g<v>))"#)?;
    let mut seen_captures = HashSet::<&str>::new();

    fn emit_obj_match<'a>(seen_captures: &mut HashSet<&'a str>, writer: &mut impl std::fmt::Write, obj: ObjectMatch<'a>) -> Result<(), CompileError> {
        let mut seen_fields = HashSet::new();
        writer.write_str(r#"(?:\g<f>,)*?"#)?;
        for (i, field) in obj.iter().enumerate() {
            if !seen_fields.insert(field.name) {
                return Err(CompileError::DuplicateFieldName)
            }
            if i > 0 {
                writer.write_str(r#",(?:\g<f>,)*?"#)?;
            }
            writer.write_str(&fancy_regex::escape(&serde_json::to_string(field.name).expect("Skill issue buckaroo")))?;
            writer.write_char(':')?;
            if let Some(c) = field.r#type.open_char() {
                writer.write_str(c)?;
            }
            if let Some(capture) = field.capture {
                if !WORD.is_match(capture).unwrap_or(false) {
                    return Err(CompileError::InvalidCaptureName)
                }
                if !seen_captures.insert(capture) {
                    return Err(CompileError::DuplicateCaptureName)
                }
                writer.write_str("(?<")?;
                writer.write_str(capture)?;
                writer.write_char('>')?;
            }
            if let Some(predicate) = field.predicate {
                Regex::new(predicate).map_err(CompileError::InvalidPredicate)?;
                writer.write_str("(?=")?;
                writer.write_str(predicate)?;
                writer.write_char(')')?;
            }
            match field.r#type {
                JsonType::ObjectMatch(obj) => {
                    emit_obj_match(seen_captures, writer, obj)?
                },
                JsonType::Literal(literal) => {
                    writer.write_str(&fancy_regex::escape(literal))?;
                },
                _ => {
                    writer.write_str("\\g<")?;
                    writer.write_char(field.r#type.routine_name())?;
                    writer.write_char('>')?;
                }
            }
            if let Some(_) = field.capture {
                writer.write_char(')')?;
            }
            if let Some(c) = field.r#type.close_char() {
                writer.write_str(c)?;
            }
        };
        writer.write_str(r#"(?:,\g<f>)*?"#)?;
        return Ok(());
    }
    writer.write_str("\\{")?;
    emit_obj_match(&mut seen_captures, writer, root)?;
    writer.write_str("\\}")?;
    return Ok(());
}

// #[cfg(test)]
// mod test {
//     #[test]
//     fn foo() {
//     }
// }