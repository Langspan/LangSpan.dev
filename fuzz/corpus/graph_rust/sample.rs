use std::io;

pub struct Server {
    port: u16,
}

impl Server {
    pub fn serve(&self) -> io::Result<()> {
        bind(self.port)?;
        Ok(())
    }
}

pub trait Listen {
    fn listen(&self);
}

const PORT: u16 = 8080;

fn bind(port: u16) -> io::Result<()> {
    let _ = port;
    Ok(())
}
