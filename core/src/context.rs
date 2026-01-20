use http::{response::Builder, Extensions, Request};

pub struct Context {
    pub req: Request<()>,
    pub res: Builder,
    // Extensions for platform-specific data (DB, Env, etc.)
    // Note: Request already has extensions, but we might want a separate one or just use req.extensions()
    extra: Extensions,
}

impl Context {
    pub fn new(req: Request<()>) -> Self {
        Self {
            req,
            res: Builder::new(),
            extra: Extensions::new(),
        }
    }

    pub fn extensions(&self) -> &Extensions {
        &self.extra
    }

    pub fn extensions_mut(&mut self) -> &mut Extensions {
        &mut self.extra
    }
}
