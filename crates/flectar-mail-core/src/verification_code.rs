//! One-time verification codes in received mail, such as the 123456 in
//! "Your verification code is 123456". The message list offers a found code
//! for copying instead of opening the message. A wrong code is worse than a
//! missed one, so a number counts only where the text calls it a code right
//! next to it. Dates, prices, phone numbers, order numbers, promotion codes
//! and meeting passcodes never do.

use std::collections::{HashMap, HashSet};
use std::sync::LazyLock;

/// Codes sit near the top of a message; the rest is mostly footers full of
/// addresses, phone numbers and dates.
const SCAN_CHARS: usize = 4000;
/// Input read to fill the scan, so that preheader padding, which collapses
/// to nothing, cannot push the code out of it.
const READ_CHARS: usize = 64 * 1024;
/// How far a name for the code may precede it ("code for your Acme account:
/// 123456") or follow it ("123456 is your Acme code").
const BEFORE_CHARS: usize = 160;
const AFTER_CHARS: usize = 60;
const BEFORE_WORDS: usize = 8;
const AFTER_WORDS: usize = 3;
/// A code alone on its line belongs to a name in the lines just above.
const ABOVE_LINES: usize = 3;
const ABOVE_CHARS: usize = 300;
/// Where a generic word such as "code" needs a sign of a sign-in.
const CONTEXT_CHARS: usize = 400;

/// Phrases that name a verification code on their own, lowercase. A
/// trailing `*` also matches longer forms of the last word ("kodunuz").
/// Scripts written without spaces match inside words.
const CODE_NAMES: &[&str] = &[
    // English
    "verification code",
    "verification pin",
    "verify code",
    "security code",
    "login code",
    "log-in code",
    "log in code",
    "sign-in code",
    "sign in code",
    "signin code",
    "authentication code",
    "authorization code",
    "auth code",
    "one-time code",
    "one time code",
    "one-time passcode",
    "one time passcode",
    "one-time password",
    "one time password",
    "one-time pin",
    "one time pin",
    "single-use code",
    "single use code",
    "otp",
    "otp code",
    "passcode",
    "2fa code",
    "mfa code",
    "two-factor code",
    "two-step code",
    "2-step code",
    "temporary code",
    "magic code",
    "reset code",
    "activation code",
    "approval code",
    // Spanish and Portuguese
    "código de verificación",
    "codigo de verificacion",
    "código de seguridad",
    "codigo de seguridad",
    "código de un solo uso",
    "código de inicio de sesión",
    "código de autenticación",
    "código de activación",
    "código temporal",
    "contraseña de un solo uso",
    "código de verificação",
    "codigo de verificacao",
    "código de segurança",
    "código de autenticação",
    "código de uso único",
    "código de login",
    "código de ativação",
    "senha de uso único",
    // French
    "code de vérification",
    "code de verification",
    "code de sécurité",
    "code de securite",
    "code de connexion",
    "code d'authentification",
    "code à usage unique",
    "code a usage unique",
    "code d'activation",
    "code de validation",
    "code temporaire",
    "mot de passe à usage unique",
    // Italian
    "codice di verifica",
    "codice di sicurezza",
    "codice di autenticazione",
    "codice monouso",
    "codice otp",
    "codice di attivazione",
    "codice di convalida",
    "codice temporaneo",
    // German
    "bestätigungscode",
    "sicherheitscode",
    "verifizierungscode",
    "verifikationscode",
    "authentifizierungscode",
    "anmeldecode",
    "einmalcode",
    "einmal-code",
    "einmalpasswort",
    "einmalkennwort",
    "aktivierungscode",
    "prüfcode",
    "login-code",
    // Dutch
    "verificatiecode",
    "beveiligingscode",
    "inlogcode",
    "eenmalige code",
    "activeringscode",
    "authenticatiecode",
    // Danish, Norwegian and Swedish
    "verifikationskode",
    "verifiseringskode",
    "verifikasjonskode",
    "sikkerhedskode",
    "sikkerhetskode",
    "engangskode",
    "loginkode",
    "login-kode",
    "aktiveringskode",
    "påloggingskode",
    "verifieringskod",
    "verifikationskod",
    "säkerhetskod",
    "engångskod",
    "inloggningskod",
    "aktiveringskod",
    // Finnish
    "vahvistuskoodi",
    "varmistuskoodi",
    "turvakoodi",
    "kertakäyttökoodi",
    "tunnistuskoodi",
    // Polish and Czech
    "kod weryfikacyjny",
    "kod bezpieczeństwa",
    "kod jednorazowy",
    "kod logowania",
    "kod autoryzacyjny",
    "hasło jednorazowe",
    "ověřovací kód",
    "bezpečnostní kód",
    "jednorázový kód",
    "přihlašovací kód",
    // Turkish
    "doğrulama kod*",
    "güvenlik kod*",
    "giriş kod*",
    "tek kullanımlık şifre*",
    "tek kullanımlık kod*",
    "aktivasyon kod*",
    // Russian and Ukrainian
    "код подтверждения",
    "код проверки",
    "проверочный код",
    "код безопасности",
    "одноразовый код",
    "одноразовый пароль",
    "код для входа",
    "код входа",
    "код верификации",
    "код активации",
    "код авторизации",
    "код аутентификации",
    "код підтвердження",
    "код перевірки",
    "одноразовий код",
    "код безпеки",
    "код для входу",
    // Greek
    "κωδικός επαλήθευσης",
    "κωδικός ασφαλείας",
    "κωδικός μίας χρήσης",
    // Chinese
    "验证码",
    "驗證碼",
    "校验码",
    "校驗碼",
    "动态码",
    "動態碼",
    "动态密码",
    "動態密碼",
    "安全码",
    "安全碼",
    "登录码",
    "登錄碼",
    "登入碼",
    "一次性密码",
    "一次性密碼",
    "激活码",
    "啟用碼",
    "授权码",
    "授權碼",
    "认证码",
    "認證碼",
    // Japanese
    "認証コード",
    "認証番号",
    "検証コード",
    "本人確認コード",
    "ワンタイムパスワード",
    "ワンタイムコード",
    "ログインコード",
    "パスコード",
    // Korean
    "인증번호",
    "인증 번호",
    "인증코드",
    "인증 코드",
    "보안코드",
    "보안 코드",
    "일회용 비밀번호",
    "일회용 코드",
    // Others
    "رمز التحقق",
    "كود التحقق",
    "رمز الأمان",
    "קוד אימות",
    "קוד חד פעמי",
    "सत्यापन कोड",
    "ओटीपी",
    "รหัสยืนยัน",
    "รหัสผ่านครั้งเดียว",
    "mã xác minh",
    "mã xác thực",
    "mã bảo mật",
    "mã đăng nhập",
    "mã otp",
    "kode verifikasi",
    "kode keamanan",
    "kode otp",
    "kod pengesahan",
    "kod keselamatan",
];

/// Generic words for a code. They also name promotion, error and postal
/// codes, so they count only next to a sign of a sign-in.
const CODE_WORDS: &[&str] = &[
    "code",
    "codes",
    "código",
    "codigo",
    "códigos",
    "codice",
    "kod*",
    "kode",
    "koden",
    "koodi",
    "kód*",
    "cod",
    "codul",
    "код",
    "кода",
    "кодом",
    "κωδικός",
    "コード",
    "코드",
    "mã",
    "קוד",
    "رمز",
    "कोड",
    "รหัส",
];

/// Names also used for booking references, cards and doors. They count only
/// next to a sign of a sign-in, and only for codes made of digits.
const WEAK_CODE_NAMES: &[&str] = &[
    "confirmation code",
    "access code",
    "pin",
    "pin code",
    "tan",
    "tac",
    "código de confirmación",
    "código de acceso",
    "código de confirmação",
    "código de acesso",
    "code de confirmation",
    "code d'accès",
    "codice di conferma",
    "codice di accesso",
    "zugangscode",
    "bevestigingscode",
    "toegangscode",
    "bekræftelseskode",
    "bekreftelseskode",
    "bekräftelsekod",
    "kod potwierdzający",
    "kod dostępu",
    "potvrzovací kód",
    "onay kod*",
    "erişim kod*",
    "код доступа",
    "确认码",
    "確認碼",
    "確認コード",
    "확인 코드",
    "확인코드",
    "mã xác nhận",
    "kode konfirmasi",
    "kode akses",
];

/// Signs that a message is about signing in or confirming an address,
/// which a generic word for a code needs nearby.
const SIGN_IN_WORDS: &[&str] = &[
    // English
    "verif*",
    "sign in",
    "sign-in",
    "signin",
    "signing in",
    "signed in",
    "log in",
    "log-in",
    "login",
    "logging in",
    "logged in",
    "authenticat*",
    "one-time",
    "one time",
    "single-use",
    "otp",
    "2fa",
    "mfa",
    "two-factor",
    "two factor",
    "two-step",
    "2-step",
    "identity",
    "confirm your email",
    "confirm your e-mail",
    "confirm your account",
    "confirm your identity",
    "confirm your sign",
    "confirm your login",
    "confirm it's you",
    "confirm it’s you",
    "confirm that it's you",
    "do not share",
    "don't share",
    "don’t share",
    "never share",
    "didn't request",
    "didn’t request",
    "did not request",
    // Spanish and Portuguese
    "verific*",
    "iniciar sesión",
    "inicio de sesión",
    "autentica*",
    "un solo uso",
    "identidad",
    "confirma tu correo",
    "confirmar tu correo",
    "confirma tu cuenta",
    "confirmar tu cuenta",
    "confirma tu identidad",
    "confirmar tu identidad",
    "no compartas",
    "no lo compartas",
    "iniciar sessão",
    "uso único",
    "identidade",
    "confirme seu e-mail",
    "confirme seu email",
    "confirme sua conta",
    "confirme sua identidade",
    "não compartilhe",
    "não partilhe",
    // French
    "vérif*",
    "connexion",
    "connecter",
    "authentifi*",
    "usage unique",
    "identité",
    "confirmez votre adresse",
    "confirmer votre adresse",
    "confirmez votre e-mail",
    "confirmer votre e-mail",
    "confirmez votre compte",
    "ne partagez",
    "ne communiquez",
    // Italian
    "accedere",
    "accesso",
    "monouso",
    "identità",
    "conferma il tuo indirizzo",
    "conferma la tua email",
    "conferma il tuo account",
    "non condividere",
    // German
    "bestätige deine",
    "bestätigen sie ihre",
    "e-mail-adresse bestätigen",
    "verifizier*",
    "anmeld*",
    "einloggen",
    "authentifizier*",
    "einmal*",
    "identität",
    "nicht weiter",
    "niemals weiter",
    // Dutch
    "bevestig je",
    "bevestig uw",
    "inloggen",
    "aanmelden",
    "eenmalig*",
    "identiteit",
    "niet delen",
    // Danish, Norwegian, Swedish and Finnish
    "bekræft din",
    "bekreft din",
    "bekräfta din",
    "log ind",
    "logge inn",
    "logga in",
    "engangs*",
    "engångs*",
    "vahvist*",
    "kirjaudu*",
    "kirjautu*",
    // Polish and Czech
    "weryfik*",
    "potwierdź swój",
    "potwierdź adres",
    "logowani*",
    "zaloguj*",
    "jednorazow*",
    "ověř*",
    "přihláš*",
    // Turkish
    "doğrula*",
    "giriş",
    "oturum aç*",
    "tek kullanımlık",
    "paylaşma*",
    // Russian and Ukrainian
    "подтвердите адрес",
    "подтвердите вход",
    "подтвердите свой",
    "подтверждения входа",
    "вход",
    "входа",
    "входу",
    "войти",
    "авториз*",
    "аутентиф*",
    "одноразов*",
    "никому не",
    "не сообщайте",
    "не передавайте",
    "підтвердіть",
    // Chinese, Japanese and Korean
    "验证",
    "驗證",
    "认证",
    "認證",
    "登录",
    "登錄",
    "登入",
    "请勿",
    "請勿",
    "一次性",
    "認証",
    "本人確認",
    "ログイン",
    "サインイン",
    "ワンタイム",
    "인증",
    "로그인",
    "일회용",
    // Others
    "xác minh",
    "đăng nhập",
    "verifikasi",
    "pengesahan",
    "אימות",
    "التحقق",
    "सत्यापन",
    "ยืนยัน",
];

/// A word just before a name for a code that makes it some other code:
/// "promo code", "zip code", "previous code".
const OTHER_CODES_BEFORE: &[&str] = &[
    "promo",
    "promotional",
    "promotion",
    "discount",
    "coupon",
    "voucher",
    "gift",
    "referral",
    "invite",
    "invitation",
    "redeem",
    "redemption",
    "offer",
    "sale",
    "zip",
    "postal",
    "post",
    "area",
    "country",
    "dial",
    "dialing",
    "dialling",
    "source",
    "qr",
    "bar",
    "error",
    "status",
    "exit",
    "return",
    "tracking",
    "booking",
    "reservation",
    "ticket",
    "order",
    "reference",
    "ref",
    "customer",
    "member",
    "pickup",
    "pick-up",
    "collection",
    "delivery",
    "parcel",
    "package",
    "cancellation",
    "room",
    "course",
    "class",
    "friend",
    "share",
    "product",
    "item",
    "tax",
    "swift",
    "sort",
    "bank",
    "routing",
    "card",
    "cvv",
    "cvc",
    "voicemail",
    "dress",
    "color",
    "colour",
    "html",
    "embed",
    "backup",
    "recovery",
    "meeting",
    "conference",
    "participant",
    "moderator",
    "host",
    "event",
    "seat",
    "gate",
    "door",
    "building",
    "lock",
    "locker",
    "parking",
    "wifi",
    "wi-fi",
    "network",
    "license",
    "licence",
    "serial",
    "previous",
    "old",
    "expired",
    "former",
    "rabatt",
    "gutschein",
    "aktions",
    "descuento",
    "cupón",
    "réduction",
    "sconto",
    "korting",
    "rabat",
    "indirim",
    "promosyon",
    "kupon",
    "скидка",
    "скидочный",
    "подарочный",
    "резервный",
];

/// Scripts without spaces attach the qualifier: "クーポンコード", "优惠码".
const OTHER_CODES_BEFORE_UNSPACED: &[&str] = &[
    "クーポン",
    "割引",
    "プロモーション",
    "招待",
    "优惠",
    "優惠",
    "折扣",
    "兑换",
    "兌換",
    "邀请",
    "邀請",
    "礼品",
    "禮品",
    "쿠폰",
    "할인",
    "초대",
];

/// Words just after a name for a code that make it some other code:
/// "código de descuento", "code review".
const OTHER_CODES_AFTER: &[&str] = &[
    "of conduct",
    "review*",
    "scanning",
    "owner*",
    "base",
    "change*",
    "quality",
    "coverage",
    "editor",
    "snippet*",
    "search",
    "freeze",
    "promo*",
    "postal",
    "postale",
    "de descuento",
    "de promoción",
    "de reserva",
    "de seguimiento",
    "de regalo",
    "de respaldo",
    "de recuperación",
    "de desconto",
    "de rastreamento",
    "de réduction",
    "de parrainage",
    "de secours",
    "de suivi",
    "sconto",
    "di sconto",
    "скидки",
    "на скидку",
];

/// Meeting invitations carry passcodes, which are not verification codes.
const MEETINGS: &[&str] = &[
    "meeting id",
    "meeting-id",
    "meeting number",
    "webinar id",
    "conference id",
    "zoom.us/",
    "zoom meeting",
    "teams meeting",
    "meet.google.com",
    "join the meeting",
    "join meeting",
    "dial-in",
    "dial in",
    "id de reunión",
    "id de la reunión",
    "id de réunion",
    "besprechungs-id",
    "id riunione",
    "id della riunione",
    "vergader-id",
    "møde-id",
    "toplantı kimliği",
    "идентификатор конференции",
    "会议号",
    "會議號",
    "会议 id",
    "ミーティングid",
    "ミーティング id",
    "회의 id",
];

/// Words that may sit between a name and its code: "code is 123456",
/// "123456 is your code". Articles and possessives count as none.
const LINKS: &[&str] = &[
    "is",
    "are",
    "was",
    "es",
    "son",
    "est",
    "sont",
    "ist",
    "lautet",
    "è",
    "é",
    "er",
    "är",
    "on",
    "jest",
    "je",
    "to",
    "это",
    "是",
    "为",
    "為",
    "は",
    "が",
    "below",
    "here",
    "follows",
    "following",
    "as",
    "your",
    "the",
    "a",
    "an",
    "this",
    "tu",
    "su",
    "el",
    "la",
    "votre",
    "ton",
    "ta",
    "le",
    "ihr",
    "ihre",
    "dein",
    "deine",
    "der",
    "die",
    "das",
    "il",
    "tuo",
    "tua",
    "uw",
    "de",
    "het",
    "din",
    "ditt",
    "ваш",
    "твой",
    "您的",
    "你的",
    "seu",
    "sua",
    "o",
];

/// Characters that end a sentence; a name and its code share one.
const SENTENCE_ENDS: &[char] = &['.', '!', '?', ';', '。', '！', '？', '；'];

/// Characters that may frame a code on a line of its own.
const DECORATIONS: &[char] = &[
    '*', '_', '"', '\'', '«', '»', '“', '”', '‘', '’', ':', '：', '-', '–', '—', '(', ')', '[',
    ']', '【', '】', '「', '」', '<', '>', '|', '=',
];

/// The code a received message asks its reader to enter, if any: digits as
/// written without spaces or hyphens ("123 456" gives "123456"), or capitals
/// and digits with their hyphens ("ABC-123").
pub fn find(subject: &str, text: &str) -> Option<String> {
    let subject = Scan::new(subject);
    let body = Scan::new(text);
    let subject_signs_in = subject.signs_in(0, subject.len(), None);
    let body_signs_in = body.signs_in(0, body.len().min(2 * CONTEXT_CHARS), None);
    let subject_names_code = subject
        .hits
        .iter()
        .any(|hit| !hit.other && !hit.term.generic);
    let mut best: Option<(Tie, usize, usize, String)> = None;
    for (source, scan, elsewhere_signs_in) in
        [(0, &subject, body_signs_in), (1, &body, subject_signs_in)]
    {
        let candidates = scan.candidates();
        let alone: Vec<bool> = candidates
            .iter()
            .map(|candidate| scan.alone_on_line(candidate))
            .collect();
        // Several codes on lines of their own are a list, such as backup
        // codes, rather than one code to enter.
        let listed = candidates
            .iter()
            .zip(&alone)
            .filter(|(_, alone)| **alone)
            .map(|(candidate, _)| candidate.code.as_str())
            .collect::<HashSet<_>>()
            .len()
            > 1;
        let first_alone = alone.iter().position(|alone| *alone);
        for (index, candidate) in candidates.iter().enumerate() {
            let line = alone[index] && !listed;
            let below_subject = source == 1 && subject_names_code && first_alone == Some(index);
            let Some(tie) = scan.tie(candidate, line, below_subject, elsewhere_signs_in) else {
                continue;
            };
            let better = best
                .as_ref()
                .is_none_or(|(best_tie, best_source, best_start, _)| {
                    (tie, std::cmp::Reverse((source, candidate.start)))
                        > (*best_tie, std::cmp::Reverse((*best_source, *best_start)))
                });
            if better {
                best = Some((tie, source, candidate.start, candidate.code.clone()));
            }
        }
    }
    best.filter(|_| !subject.meeting() && !body.meeting())
        .map(|(_, _, _, code)| code)
}

/// Whether a message may hold a code beyond its subject and preview: it
/// names a code or talks about signing in, so its full text is worth
/// reading when they showed none.
pub fn mentioned(subject: &str, text: &str) -> bool {
    let subject = Scan::new(subject);
    let text = Scan::new(text);
    !subject.meeting()
        && !text.meeting()
        && [&subject, &text].iter().any(|scan| {
            scan.hits.iter().any(|hit| !hit.other) || scan.signs_in(0, scan.len(), None)
        })
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Shape {
    Digits,
    /// Capitals and digits, such as "F4K2P" or "ABC-123".
    Mixed,
}

#[derive(Debug)]
struct Candidate {
    start: usize,
    end: usize,
    code: String,
    shape: Shape,
    /// 1900 to 2099, which is far more often a year.
    year: bool,
}

/// How closely the text ties a candidate to a name for a code, weakest
/// first.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
enum Tie {
    /// Alone on a line below a name.
    Line,
    /// A few words from a name.
    Near,
    /// Right next to a name: "code: 123456", "123456 is your code".
    Adjacent,
}

#[derive(Clone, Copy, Debug)]
struct Term {
    /// A generic word such as "code" also needs a sign of a sign-in.
    generic: bool,
    /// Only digits count, as for "confirmation code" or "PIN".
    digits_only: bool,
}

#[derive(Clone, Copy, Debug)]
struct Hit {
    start: usize,
    end: usize,
    term: Term,
    /// Names some other kind of code, such as "promo code".
    other: bool,
}

struct Pattern<T> {
    chars: Vec<char>,
    /// The last word may continue.
    stem: bool,
    value: T,
}

/// Lowercase phrases, indexed by their first character.
struct Lexicon<T> {
    ascii: Vec<Vec<Pattern<T>>>,
    other: HashMap<char, Vec<Pattern<T>>>,
}

impl<T: Copy> Lexicon<T> {
    fn new<'a>(entries: impl IntoIterator<Item = (&'a str, T)>) -> Self {
        let mut lexicon = Self {
            ascii: (0..128).map(|_| Vec::new()).collect(),
            other: HashMap::new(),
        };
        for (text, value) in entries {
            let chars: Vec<char> = text.trim_end_matches('*').chars().collect();
            let first = chars[0];
            let pattern = Pattern {
                stem: text.ends_with('*'),
                chars,
                value,
            };
            if first.is_ascii() {
                lexicon.ascii[first as usize].push(pattern);
            } else {
                lexicon.other.entry(first).or_default().push(pattern);
            }
        }
        lexicon
    }

    fn starting_with(&self, first: char) -> &[Pattern<T>] {
        if first.is_ascii() {
            &self.ascii[first as usize]
        } else {
            self.other.get(&first).map_or(&[], Vec::as_slice)
        }
    }

    /// The longest phrase starting at `at`, ending where its last word ends.
    /// Phrases in spaced scripts must start and, unless they are stems, end
    /// at a word boundary.
    fn longest_at(&self, text: &[char], at: usize) -> Option<(usize, T)> {
        // Nothing starts inside a word of a spaced script.
        if at > 0 && spaced_word(text[at]) && spaced_word(text[at - 1]) {
            return None;
        }
        self.starting_with(text[at])
            .iter()
            .filter_map(|pattern| {
                let mut end = at + pattern.chars.len();
                let last = pattern.chars[pattern.chars.len() - 1];
                if end > text.len() || text[at..end] != pattern.chars[..] {
                    return None;
                }
                if spaced_word(last) {
                    let continues = end < text.len() && spaced_word(text[end]);
                    if continues && !pattern.stem {
                        return None;
                    }
                    while end < text.len() && spaced_word(text[end]) {
                        end += 1;
                    }
                }
                Some((end, pattern.value))
            })
            .max_by_key(|(end, _)| *end)
    }

    fn occurs(&self, text: &[char], from: usize, to: usize) -> bool {
        (from..to.min(text.len())).any(|at| self.longest_at(text, at).is_some())
    }
}

static TERMS: LazyLock<Lexicon<Term>> = LazyLock::new(|| {
    let term = |generic, digits_only| Term {
        generic,
        digits_only,
    };
    Lexicon::new(
        CODE_NAMES
            .iter()
            .map(|text| (*text, term(false, false)))
            .chain(CODE_WORDS.iter().map(|text| (*text, term(true, false))))
            .chain(WEAK_CODE_NAMES.iter().map(|text| (*text, term(true, true)))),
    )
});
static SIGN_IN: LazyLock<Lexicon<()>> =
    LazyLock::new(|| Lexicon::new(SIGN_IN_WORDS.iter().map(|text| (*text, ()))));
static OTHER_AFTER: LazyLock<Lexicon<()>> =
    LazyLock::new(|| Lexicon::new(OTHER_CODES_AFTER.iter().map(|text| (*text, ()))));
static MEETING: LazyLock<Lexicon<()>> =
    LazyLock::new(|| Lexicon::new(MEETINGS.iter().map(|text| (*text, ()))));
static OTHER_BEFORE: LazyLock<HashSet<&str>> =
    LazyLock::new(|| OTHER_CODES_BEFORE.iter().copied().collect());
static OTHER_BEFORE_ATTACHED: LazyLock<Vec<Vec<char>>> = LazyLock::new(|| {
    OTHER_CODES_BEFORE_UNSPACED
        .iter()
        .map(|qualifier| qualifier.chars().collect())
        .collect()
});
static LINK_WORDS: LazyLock<HashSet<&str>> = LazyLock::new(|| LINKS.iter().copied().collect());
static LINK_PREFIXES: LazyLock<[Vec<char>; 3]> =
    LazyLock::new(|| ["https://", "http://", "www."].map(|prefix| prefix.chars().collect()));

/// Stands in for a link, whose ids and tokens ("?code=8F2A") are not codes
/// to type.
const LINK: char = '\u{fffc}';

/// Normalized text: single spaces, at most one blank line, invisible
/// characters and links removed, and a lowercase copy with the same
/// positions.
struct Scan {
    chars: Vec<char>,
    lower: Vec<char>,
    /// The lowercase text with its links.
    raw_lower: Vec<char>,
    hits: Vec<Hit>,
}

impl Scan {
    fn new(text: &str) -> Self {
        let mut chars: Vec<char> = Vec::with_capacity(text.len().min(SCAN_CHARS));
        for character in text.chars().take(READ_CHARS) {
            let character = match character {
                '\u{ad}' | '\u{34f}' | '\u{200b}'..='\u{200d}' | '\u{2060}' | '\u{feff}' => {
                    continue;
                }
                '\u{2028}' | '\u{2029}' => '\n',
                // Full-width digits and letters, as in "認証コード：１２３４５６".
                // Full-width punctuation keeps its meaning: "，" separates
                // clauses rather than digits.
                '０'..='９' | 'Ａ'..='Ｚ' | 'ａ'..='ｚ' => {
                    char::from_u32(u32::from(character) - 0xfee0).unwrap_or(character)
                }
                character if character != '\n' && character.is_whitespace() => ' ',
                character => character,
            };
            match character {
                ' ' if matches!(chars.last(), None | Some(' ' | '\n')) => {}
                '\n' => {
                    if chars.last() == Some(&' ') {
                        chars.pop();
                    }
                    if !chars.ends_with(&['\n', '\n']) {
                        chars.push('\n');
                    }
                }
                character => chars.push(character),
            }
            if chars.len() >= SCAN_CHARS {
                break;
            }
        }
        let raw_lower: Vec<char> = chars.iter().copied().map(lowercase).collect();
        let (chars, lower) = without_links(&chars, &raw_lower);
        let mut scan = Self {
            chars,
            lower,
            raw_lower,
            hits: Vec::new(),
        };
        scan.hits = scan.find_hits();
        scan
    }

    /// A meeting invitation, whose passcode is no verification code. Its
    /// links identify it too, so this reads the text before their removal.
    fn meeting(&self) -> bool {
        MEETING.occurs(&self.raw_lower, 0, self.raw_lower.len())
    }

    fn len(&self) -> usize {
        self.chars.len()
    }

    /// Every name for a code, leftmost and longest first, so "verification
    /// code" is one name rather than also "code".
    fn find_hits(&self) -> Vec<Hit> {
        let mut hits = Vec::new();
        let mut at = 0;
        while at < self.lower.len() {
            match TERMS.longest_at(&self.lower, at) {
                Some((end, term)) => {
                    hits.push(Hit {
                        start: at,
                        end,
                        term,
                        other: self.other_code(at, end),
                    });
                    at = end;
                }
                None => at += 1,
            }
        }
        hits
    }

    fn other_code(&self, start: usize, end: usize) -> bool {
        let lower = &self.lower;
        if !spaced_word(lower[start]) {
            return OTHER_BEFORE_ATTACHED
                .iter()
                .any(|qualifier| lower[..start].ends_with(qualifier));
        }
        let mut word_end = start;
        while word_end > 0 && matches!(lower[word_end - 1], ' ' | '-') {
            word_end -= 1;
        }
        let mut word_start = word_end;
        while word_start > 0 && spaced_word(lower[word_start - 1]) {
            word_start -= 1;
        }
        let previous: String = lower[word_start..word_end].iter().collect();
        if OTHER_BEFORE.contains(previous.as_str()) {
            return true;
        }
        let mut next = end;
        while next < lower.len() && lower[next] == ' ' {
            next += 1;
        }
        next < lower.len() && OTHER_AFTER.longest_at(lower, next).is_some()
    }

    /// Whether a sign of a sign-in occurs in the range, outside `skip`.
    fn signs_in(&self, from: usize, to: usize, skip: Option<&Hit>) -> bool {
        match skip {
            Some(hit) => {
                SIGN_IN.occurs(&self.lower, from, hit.start.max(from))
                    || SIGN_IN.occurs(&self.lower, hit.end.min(to), to)
            }
            None => SIGN_IN.occurs(&self.lower, from, to),
        }
    }

    /// Runs of ASCII letters and digits, joined by single hyphens.
    fn runs(&self) -> Vec<(usize, usize)> {
        let chars = &self.chars;
        let mut runs = Vec::new();
        let mut at = 0;
        while at < chars.len() {
            if !chars[at].is_ascii_alphanumeric() {
                at += 1;
                continue;
            }
            let start = at;
            while at < chars.len()
                && (chars[at].is_ascii_alphanumeric()
                    || (chars[at] == '-'
                        && chars.get(at + 1).is_some_and(char::is_ascii_alphanumeric)))
            {
                at += 1;
            }
            runs.push((start, at));
        }
        runs
    }

    fn candidates(&self) -> Vec<Candidate> {
        let runs = self.runs();
        let mut candidates = Vec::new();
        let mut index = 0;
        while index < runs.len() {
            let (start, end) = runs[index];
            if let Some(&(next_start, next_end)) = runs.get(index + 1)
                && next_start == end + 1
                && self.chars[end] == ' '
                && let Some(code) = pair(&self.chars[start..end], &self.chars[next_start..next_end])
            {
                if !self.number_before(start)
                    && !self.number_after(next_end)
                    && self.bounded(start, next_end)
                {
                    candidates.push(Candidate {
                        start,
                        end: next_end,
                        code,
                        shape: Shape::Digits,
                        year: false,
                    });
                }
                index += 2;
                continue;
            }
            // Every code holds a digit; most runs are words.
            if self.chars[start..end].iter().any(char::is_ascii_digit)
                && !self.number_before(start)
                && !self.number_after(end)
                && self.bounded(start, end)
            {
                candidates.extend(classify(&self.chars[start..end], start, end));
            }
            index += 1;
        }
        candidates
    }

    /// Part of a longer number: "0800 123 4567", "12.05.2024", "10:30".
    fn number_before(&self, start: usize) -> bool {
        start >= 2
            && matches!(self.chars[start - 1], ' ' | '.' | ',' | ':' | '/' | '-')
            && self.chars[start - 2].is_ascii_digit()
    }

    fn number_after(&self, end: usize) -> bool {
        end + 1 < self.len()
            && matches!(self.chars[end], ' ' | '.' | ',' | ':' | '/' | '-')
            && self.chars[end + 1].is_ascii_digit()
    }

    /// Not part of a word, an amount, a reference number or an address.
    fn bounded(&self, start: usize, end: usize) -> bool {
        let chars = &self.chars;
        let left = start == 0
            || match chars[start - 1] {
                // Emphasis, but not a masked card number such as "****1234".
                '*' => start < 2 || chars[start - 2] != '*',
                character => !spaced_word(character) && !"$€£¥₹₽₺₩#№+@&\\/%~^".contains(character),
            };
        let right = end == chars.len()
            || match chars[end] {
                '*' => chars.get(end + 1) != Some(&'*'),
                character => !spaced_word(character) && !"%‰€$£¥₹₽₺₩@&\\/+°×".contains(character),
            };
        left && right
    }

    fn alone_on_line(&self, candidate: &Candidate) -> bool {
        let framing = |character: &char| *character == ' ' || DECORATIONS.contains(character);
        let chars = &self.chars;
        let before = chars[..candidate.start]
            .iter()
            .rev()
            .find(|character| !framing(character));
        let after = chars[candidate.end..]
            .iter()
            .find(|character| !framing(character));
        matches!(before, None | Some('\n')) && matches!(after, None | Some('\n'))
    }

    /// How closely a name for a code ties itself to the candidate, if any
    /// name does and its conditions hold.
    fn tie(
        &self,
        candidate: &Candidate,
        alone: bool,
        below_subject: bool,
        elsewhere_signs_in: bool,
    ) -> Option<Tie> {
        let before = self
            .hits
            .iter()
            .rev()
            .find(|hit| hit.end <= candidate.start)
            .filter(|hit| candidate.start - hit.end <= BEFORE_CHARS);
        let after = self
            .hits
            .iter()
            .find(|hit| hit.start >= candidate.end)
            .filter(|hit| hit.start - candidate.end <= AFTER_CHARS);
        let mut ties = Vec::new();
        if let Some(hit) = before.filter(|hit| !hit.other)
            && let Some(tie) = tie_before(&self.lower[hit.end..candidate.start])
        {
            ties.push((tie, Some(*hit)));
        }
        if let Some(hit) = after.filter(|hit| !hit.other)
            && let Some(tie) = tie_after(&self.lower[candidate.end..hit.start])
        {
            ties.push((tie, Some(*hit)));
        }
        if alone && ties.is_empty() {
            match self.name_above(candidate) {
                Some(hit) => ties.push((Tie::Line, Some(hit))),
                None if below_subject => ties.push((Tie::Line, None)),
                None => {}
            }
        }
        ties.into_iter()
            .filter(|(tie, hit)| {
                if candidate.year && *tie != Tie::Adjacent {
                    return false;
                }
                let Some(hit) = hit else {
                    return true;
                };
                if hit.term.digits_only && candidate.shape != Shape::Digits {
                    return false;
                }
                !hit.term.generic
                    || elsewhere_signs_in
                    || self.signs_in(
                        candidate.start.saturating_sub(CONTEXT_CHARS),
                        candidate.end + CONTEXT_CHARS,
                        Some(hit),
                    )
            })
            .map(|(tie, _)| tie)
            .max()
    }

    /// The nearest name in the few lines above a line holding a code.
    fn name_above(&self, candidate: &Candidate) -> Option<Hit> {
        let chars = &self.chars;
        let line_start = chars[..candidate.start]
            .iter()
            .rposition(|character| *character == '\n')
            .map_or(0, |at| at + 1);
        let mut from = line_start;
        let mut lines = 0;
        while from > 0 && lines < ABOVE_LINES {
            // `from - 1` is the line break ending the line above.
            let end = from - 1;
            let start = chars[..end]
                .iter()
                .rposition(|character| *character == '\n')
                .map_or(0, |at| at + 1);
            if start < end {
                lines += 1;
            }
            from = start;
        }
        let from = from.max(candidate.start.saturating_sub(ABOVE_CHARS));
        let hit = self
            .hits
            .iter()
            .rev()
            .find(|hit| hit.end <= line_start && hit.start >= from)?;
        (!hit.other).then_some(*hit)
    }
}

/// Text from a name to a code after it: only linking words, or a few words
/// ending in a link or colon.
fn tie_before(gap: &[char]) -> Option<Tie> {
    let pieces = pieces(gap);
    // A paragraph break separates an introduction ending in a colon from
    // the code that starts the next paragraph, and nothing else.
    if let Some(blank) = pieces
        .windows(2)
        .position(|pair| matches!(pair, [Piece::Break, Piece::Break]))
    {
        let before_break = pieces[..blank]
            .iter()
            .rev()
            .find(|piece| **piece != Piece::Break);
        if !matches!(before_break, None | Some(Piece::Mark(':' | '：')))
            || pieces[blank..].iter().any(|piece| *piece != Piece::Break)
        {
            return None;
        }
    }
    let free = free_words(&pieces)?;
    if free == 0 {
        return Some(Tie::Adjacent);
    }
    let last = pieces.iter().rev().find(|piece| **piece != Piece::Break)?;
    let linked = match last {
        Piece::Mark(mark) => matches!(mark, ':' | '：' | '-' | '–' | '—' | '='),
        Piece::Word(word) => LINK_WORDS.contains(word.as_str()),
        _ => false,
    };
    (free <= BEFORE_WORDS && linked).then_some(Tie::Near)
}

/// Text from a code to a name after it: a link first, then a few words.
fn tie_after(gap: &[char]) -> Option<Tie> {
    let pieces = pieces(gap);
    if pieces.contains(&Piece::Break) {
        return None;
    }
    let linked = match pieces.first() {
        None => true,
        Some(Piece::Mark(mark)) => matches!(mark, ':' | '：' | '-' | '–' | '—' | '=' | '|' | '('),
        Some(Piece::Word(word)) => LINK_WORDS.contains(word.as_str()),
        _ => false,
    };
    if !linked {
        return None;
    }
    match free_words(&pieces)? {
        0 => Some(Tie::Adjacent),
        free if free <= AFTER_WORDS => Some(Tie::Near),
        _ => None,
    }
}

/// Words that are not links. None when the gap ends a sentence or holds a
/// number long enough to be the code instead.
fn free_words(pieces: &[Piece]) -> Option<usize> {
    let mut free = 0;
    for piece in pieces {
        match piece {
            Piece::Mark(mark) if SENTENCE_ENDS.contains(mark) => return None,
            Piece::Number(digits) if *digits >= 4 => return None,
            Piece::Number(_) => free += 1,
            Piece::Word(word) if !LINK_WORDS.contains(word.as_str()) => free += 1,
            _ => {}
        }
    }
    Some(free)
}

#[derive(Debug, PartialEq, Eq)]
enum Piece {
    Word(String),
    /// Letters and digits whose longest run of digits is this long, such
    /// as "10" or an amount like "45.00".
    Number(usize),
    Mark(char),
    Break,
}

fn pieces(text: &[char]) -> Vec<Piece> {
    let mut pieces = Vec::new();
    let mut at = 0;
    while at < text.len() {
        let character = text[at];
        if character.is_alphanumeric() {
            let start = at;
            while at < text.len()
                && (text[at].is_alphanumeric()
                    || (matches!(text[at], '\'' | '’' | '-')
                        && text.get(at + 1).is_some_and(|next| next.is_alphanumeric()))
                    || (matches!(text[at], '.' | ',')
                        && text[at - 1].is_ascii_digit()
                        && text.get(at + 1).is_some_and(char::is_ascii_digit)))
            {
                at += 1;
            }
            let run = &text[start..at];
            let digits = run
                .split(|character| !character.is_ascii_digit())
                .map(<[char]>::len)
                .max()
                .unwrap_or(0);
            pieces.push(if run.iter().any(char::is_ascii_digit) {
                Piece::Number(digits)
            } else {
                Piece::Word(run.iter().collect())
            });
            continue;
        }
        match character {
            '\n' => pieces.push(Piece::Break),
            ' ' => {}
            mark => pieces.push(Piece::Mark(mark)),
        }
        at += 1;
    }
    pieces
}

/// Replaces each web address with [`LINK`] in the text and its lowercase
/// copy, keeping the punctuation that ends its sentence.
fn without_links(chars: &[char], lower: &[char]) -> (Vec<char>, Vec<char>) {
    let mut kept = Vec::with_capacity(chars.len());
    let mut kept_lower = Vec::with_capacity(chars.len());
    let mut at = 0;
    while at < chars.len() {
        let starts_link = (at == 0 || !spaced_word(chars[at - 1]))
            && LINK_PREFIXES
                .iter()
                .any(|prefix| lower[at..].starts_with(prefix));
        if !starts_link {
            kept.push(chars[at]);
            kept_lower.push(lower[at]);
            at += 1;
            continue;
        }
        let mut end = at;
        while end < chars.len() && !chars[end].is_whitespace() && !"<>\"«»“”".contains(chars[end])
        {
            end += 1;
        }
        while end > at && ".,;:!?)]}'’".contains(chars[end - 1]) {
            end -= 1;
        }
        kept.push(LINK);
        kept_lower.push(LINK);
        at = end;
    }
    (kept, kept_lower)
}

/// Two equal groups of digits, as in "123 456" or "1234-5678", but not a
/// range of years such as "2025-2026".
fn pair(first: &[char], second: &[char]) -> Option<String> {
    let group =
        |digits: &[char]| matches!(digits.len(), 3 | 4) && digits.iter().all(char::is_ascii_digit);
    (first.len() == second.len() && group(first) && group(second) && !(year(first) && year(second)))
        .then(|| first.iter().chain(second).collect())
}

fn year(digits: &[char]) -> bool {
    digits.len() == 4
        && matches!(
            digits.iter().collect::<String>().parse::<u32>(),
            Ok(1900..=2099)
        )
}

fn classify(run: &[char], start: usize, end: usize) -> Option<Candidate> {
    let groups: Vec<&[char]> = run.split(|character| *character == '-').collect();
    let digits = |group: &[char]| group.iter().all(char::is_ascii_digit);
    let numeric = |code: String| {
        let chars: Vec<char> = code.chars().collect();
        Candidate {
            start,
            end,
            year: year(&chars),
            code,
            shape: Shape::Digits,
        }
    };
    if groups.iter().all(|group| digits(group)) {
        return match groups.as_slice() {
            [code] if (4..=8).contains(&code.len()) => Some(numeric(code.iter().collect())),
            [first, second] => pair(first, second).map(numeric),
            _ => None,
        };
    }
    // Google and Facebook prefix their codes: "G-123456".
    if let [prefix, number] = groups.as_slice()
        && (1..=2).contains(&prefix.len())
        && prefix.iter().all(char::is_ascii_uppercase)
        && (4..=8).contains(&number.len())
        && digits(number)
    {
        return Some(numeric(number.iter().collect()));
    }
    let capitals = run
        .iter()
        .filter(|character| character.is_ascii_uppercase())
        .count();
    let numerals = run
        .iter()
        .filter(|character| character.is_ascii_digit())
        .count();
    (capitals > 0
        && numerals > 0
        && capitals + numerals + groups.len() - 1 == run.len()
        && (4..=10).contains(&(capitals + numerals))
        && groups.len() <= 3
        && groups.iter().all(|group| group.len() >= 2))
    .then(|| Candidate {
        start,
        end,
        code: run.iter().collect(),
        shape: Shape::Mixed,
        year: false,
    })
}

/// A letter or digit of a script that separates words with spaces.
fn spaced_word(character: char) -> bool {
    character.is_alphanumeric() && !unspaced(character)
}

/// Chinese, Japanese, Korean and Thai run words together, so a code may
/// touch the words around it.
fn unspaced(character: char) -> bool {
    matches!(
        character,
        '\u{0e00}'..='\u{0e7f}'
            | '\u{1100}'..='\u{11ff}'
            | '\u{2e80}'..='\u{9fff}'
            | '\u{a960}'..='\u{a97f}'
            | '\u{ac00}'..='\u{d7af}'
            | '\u{f900}'..='\u{faff}'
            | '\u{ff00}'..='\u{ffef}'
            | '\u{20000}'..='\u{3134f}'
    )
}

/// Lowercase without changing positions: characters whose lowercase form
/// is longer stay as they are.
fn lowercase(character: char) -> char {
    if character.is_ascii() {
        return character.to_ascii_lowercase();
    }
    let mut lower = character.to_lowercase();
    match (lower.next(), lower.next()) {
        (Some(lower), None) => lower,
        _ => character,
    }
}

#[cfg(test)]
mod tests {
    use super::{find, mentioned};

    fn check_found(cases: &[(&str, &str, &str)]) {
        for (subject, text, expected) in cases {
            assert_eq!(
                find(subject, text).as_deref(),
                Some(*expected),
                "{subject:?} / {text:?}"
            );
        }
    }

    fn check_none(cases: &[(&str, &str)]) {
        for (subject, text) in cases {
            assert_eq!(find(subject, text), None, "{subject:?} / {text:?}");
        }
    }

    #[test]
    fn finds_codes_that_the_message_names() {
        check_found(&[
            (
                "Verify Your Identity With Email Verification Code",
                "Dear user , Verification code: 069672 . Use this to register a HUAWEI ID. For your security, do not share this code.",
                "069672",
            ),
            (
                "Your verification code",
                "Your verification code is 123456",
                "123456",
            ),
            (
                "Sign in",
                "Your sign-in code: 4821. It expires in 10 minutes.",
                "4821",
            ),
            (
                "123456 is your Instagram code",
                "Hi alex, Someone tried to log in to your Instagram account.",
                "123456",
            ),
            ("G-482913 is your Google verification code", "", "482913"),
            (
                "Your code",
                "Use the code 734 912 to finish signing in.",
                "734912",
            ),
            (
                "WhatsApp",
                "Your WhatsApp verification code is 123-456",
                "123456",
            ),
            (
                "Microsoft account security code",
                "Please use the following security code for the Microsoft account ma***@example.com.\n\nSecurity code: 1234567\n\nIf you don't recognize the Microsoft account, you can click here to remove your email address from that account.",
                "1234567",
            ),
            (
                "[GitHub] Please verify your device",
                "Hey alex!\n\nA sign in attempt requires further verification because we did not recognize your device. To complete the sign in, enter the verification code on the unrecognized device.\n\nDevice: Firefox on Ubuntu\nVerification code: 12345678\n\nIf you did not attempt to sign in to your account, your password may be compromised.",
                "12345678",
            ),
            (
                "Verify your Apple Account email address",
                "Verify your email address\nYou've selected this email address as your new Apple Account. To verify this email address belongs to you, enter the code below on the email verification page:\n\n482913\n\nThis code will expire three hours after this email was sent.",
                "482913",
            ),
            (
                "Verify your new Amazon account",
                "To verify your email address, please use the following One Time Password (OTP):\n\n583920\n\nDo not share this OTP with anyone.",
                "583920",
            ),
            (
                "Steam Guard",
                "Here is the Steam Guard code you need to login to account alex: F4K2P",
                "F4K2P",
            ),
            (
                "Slack confirmation code: 123-456",
                "Your confirmation code is below — enter it in your open browser window and we'll help you get signed in.",
                "123456",
            ),
            (
                "Netflix: Your sign-in code",
                "Enter this code to sign in\n4829\nThis code will expire in 15 minutes. If you didn't request it, you can ignore this email.",
                "4829",
            ),
            (
                "Bank alert",
                "Your one-time password for the payment of USD 45.00 at ACME STORE is 551203. Never share it.",
                "551203",
            ),
            ("", "Your verification code is *830117*", "830117"),
            (
                "",
                "Your verification code (valid for 10 minutes) is 830117",
                "830117",
            ),
            (
                "",
                "Your code is 4821 — never share it with anyone.",
                "4821",
            ),
            (
                "",
                "Verification code\n2026\nDo not share this code.",
                "2026",
            ),
            ("", "Verification code: 2026", "2026"),
            (
                "Your Notion login code",
                "Notion\n\nAB12-CD34\n\nThis code expires in 20 minutes.",
                "AB12-CD34",
            ),
            (
                "Your Discord verification code",
                "Hey alex, Your verification code is: 482913. If you didn't request this, please ignore.",
                "482913",
            ),
            (
                "Your Epic Games security code",
                "Here is the security code you requested:\n\n482913\n\nThis code expires in 1 hour.",
                "482913",
            ),
            (
                "Confirm your email",
                "Hi there,\nPlease use this code to confirm your email address:\n4829\nThanks, The Acme Team",
                "4829",
            ),
            (
                "Your one-time passcode",
                "Use the following one-time passcode (OTP) to log in: 48291375",
                "48291375",
            ),
            (
                "Binance",
                "[Binance] Verification Code: 482913 for login. The code is valid for 30 minutes.",
                "482913",
            ),
            (
                "LinkedIn",
                "Alex, here's your PIN 482913. Please complete your sign in by entering this verification code.",
                "482913",
            ),
            (
                "Your Proton verification code",
                "Your Proton verification code is:\n\n482 913\n\nThe code will expire in 10 minutes.",
                "482913",
            ),
            (
                "Revolut",
                "Use 482913 as your verification code to log in.",
                "482913",
            ),
            (
                "Instagram",
                "We received a request to reset your Instagram password. Enter the following password reset code: 482913",
                "482913",
            ),
            (
                "Login to Acme",
                "Your Acme login code\nHi Alex,\nUse the code below to log in.\n\nG7K2-PX9Q\n\nThis code expires in 10 minutes.",
                "G7K2-PX9Q",
            ),
            (
                "Two-factor authentication",
                "Your two-factor authentication code is 4829 1375",
                "48291375",
            ),
        ]);
    }

    #[test]
    fn finds_codes_in_other_languages() {
        check_found(&[
            ("Código", "Tu código de verificación es 482913", "482913"),
            (
                "Código",
                "Tu código es 482913. No lo compartas con nadie.",
                "482913",
            ),
            (
                "Código de acceso",
                "Tu código de acceso para iniciar sesión es 482913",
                "482913",
            ),
            ("Código", "Seu código de verificação é 482913", "482913"),
            (
                "Bestätigung",
                "Ihr Bestätigungscode lautet: 482913",
                "482913",
            ),
            (
                "Dein Anmeldecode",
                "Hallo! Dein Anmeldecode lautet 482913. Er ist 10 Minuten gültig.",
                "482913",
            ),
            (
                "TAN",
                "Ihre TAN für die Anmeldung lautet: 482913. Geben Sie diese niemals weiter.",
                "482913",
            ),
            ("Code", "Votre code de vérification est : 482913", "482913"),
            ("Codice", "Il tuo codice di verifica è 482913", "482913"),
            ("Code", "Uw verificatiecode is 482913", "482913"),
            (
                "Kode",
                "Din bekræftelseskode er 4829 – del den ikke. Bekræft din e-mail.",
                "4829",
            ),
            ("Kod", "Din verifieringskod är 482913", "482913"),
            ("Kod", "Twój kod weryfikacyjny to 482913", "482913"),
            ("Kod", "Doğrulama kodunuz: 482913", "482913"),
            ("Код", "Ваш код подтверждения: 482913", "482913"),
            ("Код", "Ваш код: 482913. Никому не сообщайте его.", "482913"),
            ("Ваш код", "Код для входа в аккаунт: 482913", "482913"),
            ("验证码", "您的验证码是：482913，5分钟内有效。", "482913"),
            ("认证", "【华为】验证码482913，请勿泄露。", "482913"),
            ("認証コード", "認証コードは482913です。", "482913"),
            (
                "認証コード",
                "認証コード：１２３４５６\n有効期限は10分です。",
                "123456",
            ),
            ("인증번호", "인증번호 [482913]을 입력해 주세요.", "482913"),
        ]);
    }

    #[test]
    fn ignores_numbers_that_are_not_codes() {
        check_none(&[
            (
                "Your weekly traffic report",
                "Requests were up 18% across flectar.com in 2026…",
            ),
            (
                "Order #48213901 confirmed",
                "Thanks for your order! Order number: 48213901. Total: $123.45",
            ),
            (
                "Your order has shipped",
                "Tracking number: 1Z999AA10123456784. Call 1-800-123-4567 with questions.",
            ),
            ("Sale", "Use code SAVE20 at checkout to get 20% off."),
            (
                "Sale",
                "Use code 2468 at checkout and save on your next order.",
            ),
            (
                "Weekend offer",
                "Enter promo code 4821 at checkout. Offer expires Sunday.",
            ),
            (
                "Statement",
                "Your card ending in 1234 was charged $56.78 on 05/12/2026.",
            ),
            (
                "Address",
                "Your postal code 10115 is outside our delivery area.",
            ),
            (
                "Invitation: Weekly sync",
                "Join Zoom Meeting\nhttps://zoom.us/j/123456789\nMeeting ID: 123 456 789\nPasscode: 482913",
            ),
            (
                "Microsoft Teams meeting",
                "Join on your computer, mobile app or room device\nMeeting ID: 482 913 750 22\nPasscode: 482913",
            ),
            (
                "Invitation: Design review",
                "Join with Google Meet: meet.google.com/abc-defg-hij\nJoin by phone: (US) +1 555-123-4567 PIN: 482913#",
            ),
            (
                "Review requested",
                "@alex requested your review on #4821: Fix code review comments",
            ),
            (
                "Backup codes",
                "Your backup codes:\n\n1234 5678\n\n2345 6789\n\n3456 7890",
            ),
            (
                "Your recovery code",
                "Keep this recovery code somewhere safe: 4821-9375",
            ),
            (
                "Phone",
                "Your verification code was sent to +44 20 7946 0958.",
            ),
            (
                "Error",
                "The server returned error code 5023 while syncing.",
            ),
            (
                "Copyright",
                "© 2026 Acme Inc. Your code was deployed successfully.",
            ),
            (
                "Hotel",
                "Your booking is confirmed. Reservation code: ABC123",
            ),
            (
                "Flight",
                "Booking reference: QX7T2B. Please log in to manage your booking. Confirmation code: QX7T2B",
            ),
            (
                "Booking.com",
                "Your booking is confirmed. Confirmation number: 4829.137.502. PIN code: 4829.",
            ),
            (
                "CVV",
                "Enter the card security code 123 from the back of your card.",
            ),
            ("Coupon", "クーポンコード: 482913 をご利用ください。"),
            ("Old code", "Your previous code 482913 has expired."),
            (
                "Timing",
                "The meeting moved to 14:30 on 2026-11-02; the code freeze starts at 1800.",
            ),
            (
                "New sign-in to your account",
                "We noticed a new sign-in to your account. Check activity: https://myaccount.google.com/notifications?code=8F2A",
            ),
            (
                "Reset your password",
                "Click the link below to reset it: https://acme.com/reset/482913abc. This link expires in 24 hours.",
            ),
            (
                "Your Uber receipt",
                "Total €23.40. Trip on October 12, 2026 at 18:42. Pickup: Calle Mayor 12, 28013 Madrid.",
            ),
            (
                "Your Amazon.com order #112-4829137-5512345",
                "Arriving Tuesday. Item subtotal: $49.99. Promo code applied: SAVE10",
            ),
            (
                "Re: Wi-Fi at the office",
                "Hi! The wifi code is 48291375 and the door code is 4829. See you tomorrow.",
            ),
            (
                "Gift card",
                "Your gift code: ABCD-1234. Redeem at acme.com/redeem.",
            ),
            (
                "CI failed",
                "Build #4829 failed on main. Exit code 137 after 1820 seconds.",
            ),
            (
                "Security alert",
                "Your password was changed on October 24, 2026 at 09:41. If this wasn't you, contact support at 1-800-482-9137.",
            ),
            (
                "Newsletter",
                "Our new product line launches 2026. Use code 2026 for early access.",
            ),
            (
                "Your parcel is ready",
                "Your pickup code is 482913. Log in to the app to see your parcel.",
            ),
            (
                "Your order",
                "Order code: 482913. Sign in to track your order.",
            ),
            ("Rewards", "Log in to redeem code 4829 for a free drink."),
            ("Event", "Your access code for the conference is 4829."),
            ("Lockers", "Your PIN for the parcel locker is 4829."),
            (
                "Classroom",
                "Join my class with class code ABC123. Sign in with your school account.",
            ),
            (
                "Year in review",
                "Your 2026 in review. Sign in to see your top songs. Code: 2025-2026",
            ),
            ("", "Your verification code is below.\n\n2026\n"),
        ]);
    }

    #[test]
    fn prefers_the_code_next_to_its_name() {
        check_found(&[
            (
                "",
                "Reference 48213901. Your verification code is 551203.",
                "551203",
            ),
            (
                "",
                "Your previous code 482913 has expired. Your new verification code is 551203.",
                "551203",
            ),
            (
                "Your login code: 111111",
                "Your login code: 111111",
                "111111",
            ),
        ]);
    }

    #[test]
    fn reads_html_text_within_the_scan_window() {
        let padding = "\u{200c}\u{a0}".repeat(400);
        let text = format!(
            "Your code is 482913{padding}\nAcme\nUse this code to verify your email: 482913"
        );
        assert_eq!(find("Verify your email", &text).as_deref(), Some("482913"));
        // Footers past the scanned start never produce codes.
        let footer = format!(
            "Welcome to Acme!{}\nVerification code: 482913",
            " word".repeat(2000)
        );
        assert_eq!(find("Welcome", &footer), None);
    }

    #[test]
    fn notices_messages_that_may_hold_codes() {
        assert!(mentioned("Your verification code", ""));
        assert!(mentioned("Welcome", "Use this code to sign in"));
        assert!(mentioned("認証コード", ""));
        // A preview cut off before the code still asks for the full text.
        assert!(mentioned(
            "[GitHub] Please verify your device",
            "Hey alex! A sign in attempt requires further verification because we did not recognize your device. To complete the sign in, enter the"
        ));
        assert!(!mentioned("Your weekly report", "Requests were up 18%"));
        assert!(!mentioned("Sale", "Enter promo code SAVE20"));
        assert!(!mentioned(
            "Invitation",
            "Meeting ID: 123 456 789 Passcode: 482913"
        ));
    }
}
