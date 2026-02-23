use std::path::PathBuf;

use bon::bon;

use crate::ast;
use crate::tokenizer::{Token, TokenEndReason, Tokenizer, TokenizerOptions, Tokens};

pub mod peg;
#[cfg(feature = "winnow-parser")]
pub mod winnow_str;

/// Parser implementation to use
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq, Default)]
pub enum ParserImpl {
    /// PEG-based parser (token-based)
    #[default]
    Peg,
    /// Winnow-based parser (string-based, direct)
    #[cfg(feature = "winnow-parser")]
    Winnow,
}

/// Options used to control the behavior of the parser.
#[derive(Clone, Eq, Hash, PartialEq)]
pub struct ParserOptions {
    /// Whether or not to enable extended globbing (a.k.a. `extglob`).
    pub enable_extended_globbing: bool,
    /// Whether or not to enable POSIX compliance mode.
    pub posix_mode: bool,
    /// Whether or not to enable maximal compatibility with the `sh` shell.
    pub sh_mode: bool,
    /// Whether or not to perform tilde expansion for tildes at the start of words.
    pub tilde_expansion_at_word_start: bool,
    /// Whether or not to perform tilde expansion for tildes after colons.
    pub tilde_expansion_after_colon: bool,
    /// Select the parser internal implementation
    pub parser_impl: ParserImpl,
}

impl Default for ParserOptions {
    fn default() -> Self {
        Self {
            enable_extended_globbing: true,
            posix_mode: false,
            sh_mode: false,
            tilde_expansion_at_word_start: true,
            tilde_expansion_after_colon: false,
            parser_impl: ParserImpl::default(),
        }
    }
}

impl ParserOptions {
    /// Returns the tokenizer options implied by these parser options.
    pub const fn tokenizer_options(&self) -> TokenizerOptions {
        TokenizerOptions {
            enable_extended_globbing: self.enable_extended_globbing,
            posix_mode: self.posix_mode,
            sh_mode: self.sh_mode,
        }
    }
}

/// Information about the source of tokens.
#[derive(Clone, Debug, Default)]
#[allow(dead_code)]
pub struct SourceInfo {
    /// The source of the tokens.
    pub source: String,
}

impl From<PathBuf> for SourceInfo {
    fn from(path: PathBuf) -> Self {
        Self {
            source: path.to_string_lossy().to_string(),
        }
    }
}

/// Implements parsing for shell programs.
pub struct Parser<R: std::io::BufRead> {
    /// The reader to use for input
    reader: R,
    /// Parsing options
    options: ParserOptions,
}

#[bon]
impl<R: std::io::BufRead> Parser<R> {
    ///
    /// # Arguments
    ///
    /// * `reader` - The reader to use for input.
    /// * `options` - The options to use when parsing.
    pub fn new(reader: R, options: &ParserOptions) -> Self {
        Self {
            reader,
            options: options.clone(),
        }
    }

    /// Create a new parser instance through a builder
    #[builder(
        finish_fn(doc {
            /// Instantiate a parser with the provided reader as input
        })
    )]
    pub const fn builder(
        /// The reader to use for input
        #[builder(finish_fn)]
        reader: R,

        #[builder(default = true)]
        /// Whether or not to enable extended globbing (a.k.a. `extglob`).
        enable_extended_globbing: bool,
        #[builder(default = false)]
        /// Whether or not to enable POSIX compliance mode.
        posix_mode: bool,
        #[builder(default = false)]
        /// Whether or not to enable maximal compatibility with the `sh` shell.
        sh_mode: bool,
        #[builder(default = true)]
        /// Whether or not to perform tilde expansion for tildes at the start of words.
        tilde_expansion_at_word_start: bool,
        #[builder(default = false)]
        /// Whether or not to perform tilde expansion for tildes after colons.
        tilde_expansion_after_colon: bool,
        #[builder(default)]
        /// Select the parser internal implementation
        parser_impl: ParserImpl,
    ) -> Self {
        let options = ParserOptions {
            enable_extended_globbing,
            posix_mode,
            sh_mode,
            tilde_expansion_at_word_start,
            tilde_expansion_after_colon,
            parser_impl,
        };
        Self { reader, options }
    }

    /// Parses the input into an abstract syntax tree (AST) of a shell program.
    pub fn parse_program(&mut self) -> Result<ast::Program, crate::error::ParseError> {
        //
        // References:
        //   * https://www.gnu.org/software/bash/manual/bash.html#Shell-Syntax
        //   * https://mywiki.wooledge.org/BashParser
        //   * https://aosabook.org/en/v1/bash.html
        //   * https://pubs.opengroup.org/onlinepubs/9699919799/utilities/V3_chap02.html
        //
        let input = self.read_and_preprocess_input()?;

        match self.options.parser_impl {
            ParserImpl::Peg => {
                let tokens = tokenize_preprocessed_input(&input, &self.options)?;
                parse_tokens(&tokens, &self.options)
            }
            #[cfg(feature = "winnow-parser")]
            ParserImpl::Winnow => {
                winnow_str::parse_program(&input, &self.options, &SourceInfo::default()).map_err(
                    |_e| {
                        // Convert winnow error to ParseError
                        // TODO: Extract position information from winnow error
                        crate::error::ParseError::ParsingAtEndOfInput
                    },
                )
            }
        }
    }

    /// Parses a function definition body from the input. The body is expected to be
    /// preceded by "()", but no function name.
    pub fn parse_function_parens_and_body(
        &mut self,
    ) -> Result<ast::FunctionBody, crate::error::ParseError> {
        let input = self.read_and_preprocess_input()?;
        let tokens = tokenize_preprocessed_input(&input, &self.options)?;
        let parse_result =
            peg::token_parser::function_parens_and_body(&Tokens { tokens: &tokens }, &self.options);
        parse_result_to_error(parse_result, &tokens)
    }

    fn read_and_preprocess_input(&mut self) -> Result<String, crate::error::ParseError> {
        let mut input = String::new();
        std::io::Read::read_to_string(&mut self.reader, &mut input).map_err(|e| {
            crate::error::ParseError::Tokenizing {
                inner: crate::tokenizer::TokenizerError::from(e),
                position: None,
            }
        })?;

        preprocess_python_blocks(&input).map_err(|inner| {
            let position = match &inner {
                crate::tokenizer::TokenizerError::UnterminatedPythonBlock(pos) => Some(pos.clone()),
                _ => None,
            };
            crate::error::ParseError::Tokenizing { inner, position }
        })
    }
}

fn tokenize_preprocessed_input(
    input: &str,
    options: &ParserOptions,
) -> Result<Vec<Token>, crate::error::ParseError> {
    // First we tokenize the input, according to the policy implied by provided options.
    let mut reader = std::io::BufReader::new(input.as_bytes());
    let mut tokenizer = Tokenizer::new(&mut reader, &options.tokenizer_options());

    tracing::debug!(target: "tokenize", "Tokenizing...");

    let mut tokens = vec![];
    loop {
        let result = match tokenizer.next_token() {
            Ok(result) => result,
            Err(e) => {
                return Err(crate::error::ParseError::Tokenizing {
                    inner: e,
                    position: tokenizer.current_location(),
                });
            }
        };

        let reason = result.reason;
        if let Some(token) = result.token {
            tracing::debug!(target: "tokenize", "TOKEN {}: {:?} {reason:?}", tokens.len(), token);
            tokens.push(token);
        }

        if matches!(reason, TokenEndReason::EndOfInput) {
            break;
        }
    }

    tracing::debug!(target: "tokenize", "  => {} token(s)", tokens.len());

    Ok(tokens)
}

fn preprocess_python_blocks(input: &str) -> Result<String, crate::tokenizer::TokenizerError> {
    let mut output = String::with_capacity(input.len() + 64);
    let lines = input.split_inclusive('\n').collect::<Vec<_>>();

    let mut i = 0usize;
    let mut line_no = 1usize;
    while i < lines.len() {
        let line = lines[i];

        if let Some(insert_at) = find_python_block_insert(line) {
            let start_line = line_no;
            output.push_str(&line[..insert_at]);
            output.push_str(" <<'END_PYTHON'");
            output.push_str(&line[insert_at..]);

            i += 1;
            line_no += 1;

            let mut found_end = false;
            while i < lines.len() {
                let body_line = lines[i];
                output.push_str(body_line);
                if body_line == "END_PYTHON\n" || body_line == "END_PYTHON" {
                    found_end = true;
                    i += 1;
                    line_no += 1;
                    break;
                }
                i += 1;
                line_no += 1;
            }

            if !found_end {
                return Err(crate::tokenizer::TokenizerError::UnterminatedPythonBlock(
                    crate::SourcePosition {
                        index: 0,
                        line: start_line,
                        column: 1,
                    },
                ));
            }

            continue;
        }

        output.push_str(line);
        i += 1;
        line_no += 1;
    }

    Ok(output)
}

fn find_python_block_insert(line: &str) -> Option<usize> {
    let mut line_end = line.len();
    if line.ends_with('\n') {
        line_end -= 1;
    }
    let line_no_nl = &line[..line_end];
    if line_no_nl.trim().is_empty() {
        return None;
    }

    let pipes = unquoted_pipe_positions(line_no_nl);
    let mut segment_starts = Vec::with_capacity(pipes.len() + 1);
    let mut segment_ends = Vec::with_capacity(pipes.len() + 1);
    let mut start = 0usize;
    for p in &pipes {
        segment_starts.push(start);
        segment_ends.push(*p);
        start = p + 1;
    }
    segment_starts.push(start);
    segment_ends.push(line_no_nl.len());

    let mut insert_at: Option<usize> = None;
    for (seg_start, seg_end) in segment_starts.into_iter().zip(segment_ends) {
        let segment = &line_no_nl[seg_start..seg_end];
        if segment_is_python_command(segment) {
            if insert_at.is_some() {
                // Multiple PYTHON segments on one line are ambiguous for block collection.
                return None;
            }
            insert_at = Some(seg_end);
        }
    }

    insert_at.map(|idx| {
        if idx == line_no_nl.len() {
            line_end
        } else {
            idx
        }
    })
}

fn segment_is_python_command(segment: &str) -> bool {
    let s = segment.trim_start();
    if s.starts_with('#') || !s.starts_with("PYTHON") {
        return false;
    }
    let rest = &s["PYTHON".len()..];
    rest.chars()
        .next()
        .is_none_or(|c| c.is_whitespace() || matches!(c, '<' | '>'))
}

fn unquoted_pipe_positions(s: &str) -> Vec<usize> {
    let mut positions = Vec::new();
    let mut chars = s.char_indices().peekable();
    let mut in_single = false;
    let mut in_double = false;
    let mut escaped = false;

    while let Some((idx, ch)) = chars.next() {
        if escaped {
            escaped = false;
            continue;
        }

        if ch == '\\' && !in_single {
            escaped = true;
            continue;
        }

        if ch == '\'' && !in_double {
            in_single = !in_single;
            continue;
        }
        if ch == '"' && !in_single {
            in_double = !in_double;
            continue;
        }

        if !in_single && !in_double && ch == '|' {
            // Ignore |&; this transform only targets pipeline separators.
            if chars.peek().is_some_and(|(_, next)| *next == '&') {
                continue;
            }
            positions.push(idx);
        }
    }

    positions
}

/// Parses a sequence of tokens into the abstract syntax tree (AST) of a shell program.
///
/// # Arguments
///
/// * `tokens` - The tokens to parse.
/// * `options` - The options to use when parsing.
pub fn parse_tokens(
    tokens: &[Token],
    options: &ParserOptions,
) -> Result<ast::Program, crate::error::ParseError> {
    let parse_result = peg::token_parser::program(&Tokens { tokens }, options);
    parse_result_to_error(parse_result, tokens)
}

fn parse_result_to_error<R>(
    parse_result: Result<R, ::peg::error::ParseError<usize>>,
    tokens: &[Token],
) -> Result<R, crate::error::ParseError>
where
    R: std::fmt::Debug,
{
    match parse_result {
        Ok(program) => {
            tracing::debug!(target: "parse", "PROG: {:?}", program);
            Ok(program)
        }
        Err(parse_error) => {
            tracing::debug!(target: "parse", "Parse error: {:?}", parse_error);
            Err(crate::error::convert_peg_parse_error(&parse_error, tokens))
        }
    }
}

#[cfg(test)]
mod tests;
