use rquickjs::{Ctx, Result, String, class::Trace};

// https://webidl.spec.whatwg.org/#idl-DOMException
#[derive(Trace, rquickjs::JsLifetime)]
pub struct JsDomException<'js> {
    pub(crate) name: String<'js>,
    pub(crate) message: String<'js>,
}

impl<'js> dom_exception_generated::DOMException<'js> for JsDomException<'js> {
    // https://webidl.spec.whatwg.org/#dom-domexception-domexception
    fn constructor(_ctx: &Ctx<'js>, message: String<'js>, name: String<'js>) -> Result<Self> {
        Ok(Self { name, message })
    }

    fn get_name(&self, _ctx: &Ctx<'js>) -> Result<String<'js>> {
        Ok(self.name.clone())
    }

    fn get_message(&self, _ctx: &Ctx<'js>) -> Result<String<'js>> {
        Ok(self.message.clone())
    }

    // https://webidl.spec.whatwg.org/#dom-domexception-code
    fn get_code(&self, _ctx: &Ctx<'js>) -> Result<u16> {
        let name = match self.name.to_string() {
            Ok(name) => name,
            Err(rquickjs::Error::Utf8(_)) => return Ok(0),
            Err(error) => return Err(error),
        };
        Ok(match name.as_str() {
            "IndexSizeError" => 1,
            "HierarchyRequestError" => 3,
            "WrongDocumentError" => 4,
            "InvalidCharacterError" => 5,
            "NoModificationAllowedError" => 7,
            "NotFoundError" => 8,
            "NotSupportedError" => 9,
            "InUseAttributeError" => 10,
            "InvalidStateError" => 11,
            "SyntaxError" => 12,
            "InvalidModificationError" => 13,
            "NamespaceError" => 14,
            "InvalidAccessError" => 15,
            "TypeMismatchError" => 17,
            "SecurityError" => 18,
            "NetworkError" => 19,
            "AbortError" => 20,
            "URLMismatchError" => 21,
            "QuotaExceededError" => 22,
            "TimeoutError" => 23,
            "InvalidNodeTypeError" => 24,
            "DataCloneError" => 25,
            _ => 0,
        })
    }
}

include!(concat!(env!("OUT_DIR"), "/DOMException.rs"));
