//! Simple HTTP client

use crate::io::{decode_response, encode_request};
use crate::model::{
    HeaderName, HeaderValue, InvalidHeader, Method, Request, Response, Status, Url,
};
use crate::utils::{invalid_data_error, invalid_input_error};
// #[cfg(any(feature = "native-tls", feature = "rustls"))]
// use lazy_static::lazy_static;
// #[cfg(feature = "native-tls")]
// use native_tls::TlsConnector;
// #[cfg(feature = "rustls")]
// use rustls_crate::{ClientConfig, ClientConnection, RootCertStore, ServerName, StreamOwned};
// #[cfg(feature = "rustls")]
// use rustls_native_certs::load_native_certs;
use std::convert::TryFrom;
use std::io::{BufReader, BufWriter, Error, ErrorKind, Result};
use std::net::SocketAddr;
// use std::net::{SocketAddr, TcpStream};
// #[cfg(any(feature = "native-tls", feature = "rustls"))]
// use std::sync::Arc;
use std::time::Duration;
use lunatic::net::{TcpStream, SocketAddrIterator, TlsStream};
// use std::net::SocketAddr;

// #[cfg(feature = "rustls")]
// lazy_static! {
//     static ref RUSTLS_CONFIG: Arc<ClientConfig> = {
//         let mut root_store = RootCertStore::empty();
//         match load_native_certs() {
//             Ok(certs) => {
//                 for cert in certs {
//                     root_store.add_parsable_certificates(&[cert.0]);
//                 }
//             }
//             Err(e) => panic!("Error loading TLS certificates: {}", e),
//         }
//         Arc::new(
//             ClientConfig::builder()
//                 .with_safe_defaults()
//                 .with_root_certificates(root_store)
//                 .with_no_client_auth(),
//         )
//     };
// }

// #[cfg(feature = "native-tls")]
// lazy_static! {
//     static ref TLS_CONNECTOR: TlsConnector = {
//         match TlsConnector::new() {
//             Ok(connector) => connector,
//             Err(e) => panic!("Error while loading TLS configuration: {}", e),
//         }
//     };
// }

/// A simple HTTP client.
///
/// It aims at following the basic concepts of the [Web Fetch standard](https://fetch.spec.whatwg.org/) without the bits specific to web browsers (context, CORS...).
///
/// HTTPS is supported behind the disabled by default `native-tls` feature (to use the current system native implementation) or `rustls` feature (to use [Rustls](https://github.com/rustls/rustls)).
///
/// The client does not follow redirections by default. Use [`Client::set_redirection_limit`] to set a limit to the number of consecutive redirections the server should follow.
///
/// Missing: HSTS support, authentication and keep alive.
///
/// ```
/// use oxhttp::Client;
/// use oxhttp::model::{Request, Method, Status, HeaderName};
/// use std::io::Read;
///
/// let client = Client::new();
/// let response = client.request(Request::builder(Method::GET, "http://example.com".parse()?).build())?;
/// assert_eq!(response.status(), Status::OK);
/// assert_eq!(response.header(&HeaderName::CONTENT_TYPE).unwrap().as_ref(), b"text/html; charset=UTF-8");
/// let body = response.into_body().to_string()?;
/// # Result::<_,Box<dyn std::error::Error>>::Ok(())
/// ```
#[derive(Default)]
pub struct Client {
    timeout: Option<Duration>,
    user_agent: Option<HeaderValue>,
    redirection_limit: usize,
}

impl Client {
    #[inline]
    pub fn new() -> Self {
        Self::default()
    }

    /// Sets the global timout value (applies to both read, write and connection).
    #[inline]
    pub fn set_global_timeout(&mut self, timeout: Duration) {
        self.timeout = Some(timeout);
    }

    /// Sets the default value for the [`User-Agent`](https://httpwg.org/http-core/draft-ietf-httpbis-semantics-latest.html#field.user-agent) header.
    #[inline]
    pub fn set_user_agent(
        &mut self,
        user_agent: impl Into<String>,
    ) -> std::result::Result<(), InvalidHeader> {
        self.user_agent = Some(HeaderValue::try_from(user_agent.into())?);
        Ok(())
    }

    /// Sets the number of time a redirection should be followed.
    /// By default the redirections are not followed (limit = 0).
    #[inline]
    pub fn set_redirection_limit(&mut self, limit: usize) {
        self.redirection_limit = limit;
    }

    pub fn request(&self, mut request: Request) -> Result<Response> {
        // Loops the number of allowed redirections + 1
        for _ in 0..(self.redirection_limit + 1) {
            let previous_method = request.method().clone();
            let response = self.single_request(&mut request)?;
            if let Some(location) = response.header(&HeaderName::LOCATION) {
                let new_method = match response.status() {
                    Status::MOVED_PERMANENTLY | Status::FOUND | Status::SEE_OTHER => {
                        if previous_method == Method::HEAD {
                            Method::HEAD
                        } else {
                            Method::GET
                        }
                    }
                    Status::TEMPORARY_REDIRECT | Status::PERMANENT_REDIRECT
                        if previous_method.is_safe() =>
                    {
                        previous_method
                    }
                    _ => return Ok(response),
                };
                let location = location.to_str().map_err(invalid_data_error)?;
                let new_url = request.url().join(location).map_err(|e| {
                    invalid_data_error(format!(
                        "Invalid URL in Location header raising error {}: {}",
                        e, location
                    ))
                })?;
                let mut request_builder = Request::builder(new_method, new_url);
                for (header_name, header_value) in request.headers() {
                    request_builder
                        .headers_mut()
                        .set(header_name.clone(), header_value.clone());
                }
                request = request_builder.build();
            } else {
                return Ok(response);
            }
        }
        Err(Error::new(
            ErrorKind::Other,
            format!(
                "The server requested too many redirects ({}). The latest redirection target is {}",
                self.redirection_limit + 1,
                request.url()
            ),
        ))
    }

    #[allow(unreachable_code, clippy::needless_return)]
    fn single_request(&self, request: &mut Request) -> Result<Response> {
        // panic!("{}", request.url());

        // Additional headers
        set_header_fallback(request, HeaderName::USER_AGENT, &self.user_agent);
        request
            .headers_mut()
            .set(HeaderName::CONNECTION, HeaderValue::new_unchecked("close"));
        // #[cfg(any(feature = "native-tls", feature = "rustls"))]
        // let host = request
        //     .url()
        //     .host_str()
        //     .ok_or_else(|| invalid_input_error("No host provided"))?;

        match request.url().scheme() {
            "http" => {
                let addresses = get_and_validate_socket_addresses(request.url(), 80)?;
                let mut stream = self.connect_tcp(addresses)?;
                encode_request(request, BufWriter::new(&mut stream))?;
                decode_response(BufReader::new(stream))

            }
            "https" => {
                let addresses = get_and_validate_socket_addresses(request.url(), 443)?;
                let mut stream = self.connect_tls(request.url())?;
                encode_request(request, BufWriter::new(&mut stream))?;
                decode_response(BufReader::new(stream))
                // #[cfg(feature = "native-tls")]
                // {
                //     let addresses = get_and_validate_socket_addresses(request.url(), 443)?;
                //     let stream = self.connect(&addresses)?;
                //     let mut stream = TLS_CONNECTOR
                //         .connect(host, stream)
                //         .map_err(|e| Error::new(ErrorKind::Other, e))?;
                //     encode_request(request, BufWriter::new(&mut stream))?;
                //     return decode_response(BufReader::new(stream));
                // }
                // #[cfg(feature = "rustls")]
                // {
                //     let addresses = get_and_validate_socket_addresses(request.url(), 443)?;
                //     let dns_name = ServerName::try_from(host).map_err(invalid_input_error)?;
                //     let connection = ClientConnection::new(RUSTLS_CONFIG.clone(), dns_name)
                //         .map_err(|e| Error::new(ErrorKind::Other, e))?;
                //     let mut stream = StreamOwned::new(connection, self.connect(&addresses)?);
                //     encode_request(request, BufWriter::new(&mut stream))?;
                //     return decode_response(BufReader::new(stream));
                // }
                // #[cfg(not(any(feature = "native-tls", feature = "rustls")))]
                // return Err(invalid_input_error("HTTPS is not supported by the client. You should enable the `native-tls` or `rustls` feature of the `oxhttp` crate"));
            }
            _ => Err(invalid_input_error(format!(
                "Not supported URL scheme: {}",
                request.url().scheme()
            ))),
        }
    }

    fn connect_tcp(&self, addresses: SocketAddrIterator) -> Result<TcpStream> {
        let mut stream = addresses.fold(Err(Error::new(
            ErrorKind::InvalidInput,
            "Not able to resolve the provide addresses",
        )),
        |e, addr| match e {
            Ok(stream) => Ok(stream),
            Err(_) => if let Some(timeout) = self.timeout {
                TcpStream::connect_timeout(addr.clone(), timeout)
            } else {
                TcpStream::connect(addr.clone())
            },
        })?;

        // stream.set_read_timeout(self.timeout)?;
        // stream.set_write_timeout(self.timeout)?;
        Ok(stream)
    }

    fn connect_tls(&self, url: &Url) -> Result<TlsStream> {
        let host = match url.host() {
            Some(x)=> x,
            None=> {
                return Err(Error::new(
                    ErrorKind::InvalidInput,
                    "Not able to resolve the provide addresses",
                ))
            }
        };

        let port: u32 = match url.port() {
            Some(x)=> x.into(),
            None => 443
        };



        let stream = if let Some(timeout) = self.timeout {

            TlsStream::connect_timeout(host.to_string().as_str(), timeout, port, vec![])
        } else {
            TlsStream::connect(host.to_string().as_str(), port)

        }?;

        // stream.set_read_timeout(self.timeout)?;
        // stream.set_write_timeout(self.timeout)?;
        Ok(stream)
    }
}

// Bad ports https://fetch.spec.whatwg.org/#bad-port
// Should be sorted
// const BAD_PORTS: [u16; 80] = [
//     1, 7, 9, 11, 13, 15, 17, 19, 20, 21, 22, 23, 25, 37, 42, 43, 53, 69, 77, 79, 87, 95, 101, 102,
//     103, 104, 109, 110, 111, 113, 115, 117, 119, 123, 135, 137, 139, 143, 161, 179, 389, 427, 465,
//     512, 513, 514, 515, 526, 530, 531, 532, 540, 548, 554, 556, 563, 587, 601, 636, 989, 990, 993,
//     995, 1719, 1720, 1723, 2049, 3659, 4045, 5060, 5061, 6000, 6566, 6665, 6666, 6667, 6668, 6669,
//     6697, 10080,
// ];

fn get_and_validate_socket_addresses(url: &Url, default_port: u16) -> Result<SocketAddrIterator> {
    // let addresses = url.socket_addrs(|| Some(default_port))?;

    let port = if let Some(port) = url.port() { port } else {default_port};
    let host = match url.host_str() {
        Some(x)=> x,
        None=> {
            return Err(Error::new(
                ErrorKind::Other,
                format!(
                    "host not set"
                ),
            ))
        }
    };
    let addresses = match lunatic::net::resolve(&format!("{}:{}", host, port)) {
        Ok(x)=> x,
        Err(e)=> {
            println!("{}", e);
            return Err(Error::new(
                ErrorKind::Other,
                format!(
                    "cant DNS resole the url: {}",
                    url.host().unwrap().to_string()
                ),
            ))
        }
    };
    // for address in addresses {
    //     if BAD_PORTS.binary_search(&address.port()).is_ok() {
    //         return Err(invalid_input_error(format!(
    //             "The port {} is not allowed for HTTP(S) because it is dedicated to an other use",
    //             address.port()
    //         )));
    //     }
    // }
    Ok(addresses)
}

fn set_header_fallback(
    request: &mut Request,
    header_name: HeaderName,
    header_value: &Option<HeaderValue>,
) {
    if let Some(header_value) = header_value {
        if !request.headers().contains(&header_name) {
            request.headers_mut().set(header_name, header_value.clone())
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{Method, Status};
    use lunatic::spawn_link;
    use lunatic_test::test;
    


    #[lunatic_test::test]
    fn test_http_get_ok() {
        let client = Client::new();
        let response = client.request(
            Request::builder(Method::GET, "http://example.com".parse().unwrap()).build(),
        ).unwrap();

        assert_eq!(response.status(), Status::OK);
        assert_eq!(
            response.header(&HeaderName::CONTENT_TYPE).unwrap().as_ref(),
            b"text/html; charset=UTF-8"
        );
        
        
    }

    #[lunatic_test::test]
    fn test_http_get_ok_with_user_agent_and_timeout() {
        let mut client = Client::new();
        client.set_user_agent("OxHTTP/1.0").unwrap();
        client.set_global_timeout(Duration::from_secs(5));
        let response = client.request(
            Request::builder(Method::GET, "http://example.com".parse().unwrap()).build(),
        ).unwrap();
        assert_eq!(response.status(), Status::OK);
        assert_eq!(
            response.header(&HeaderName::CONTENT_TYPE).unwrap().as_ref(),
            b"text/html; charset=UTF-8"
        );
    }

    #[lunatic_test::test]
    fn test_http_get_ok_explicit_port() {
        let client = Client::new();
        let response = client.request(
            Request::builder(Method::GET, "http://example.com:80".parse().unwrap()).build(),
        ).unwrap();
        assert_eq!(response.status(), Status::OK);
        assert_eq!(
            response.header(&HeaderName::CONTENT_TYPE).unwrap().as_ref(),
            b"text/html; charset=UTF-8"
        );
    }

    //TODO: Implement bad port check
    // #[lunatic_test::test]
    // fn test_http_wrong_port() {
    //     let client = Client::new();
    //     assert!(client
    //         .request(
    //             Request::builder(Method::GET, "http://example.com:22".parse().unwrap()).build(),
    //         )
    //         .is_err());
    // }

    #[lunatic_test::test]
    fn test_https_get_ok() {
        let client = Client::new();

        let response = client.request(
            Request::builder(Method::GET, "https://example.com".parse().unwrap()).build(),
        ).unwrap();
        assert_eq!(response.status(), Status::OK);
        assert_eq!(
            response.header(&HeaderName::CONTENT_TYPE).unwrap().as_ref(),
            b"text/html; charset=UTF-8"
        );
    }

    #[lunatic_test::test]
    fn test_https_get_ok_with_timeout() {
        let client = Client::new();
        let response = client.request(
            Request::builder(Method::GET, "https://example.com".parse().unwrap()).build(),
        ).unwrap();
        assert_eq!(response.status(), Status::OK);
        assert_eq!(
            response.header(&HeaderName::CONTENT_TYPE).unwrap().as_ref(),
            b"text/html; charset=UTF-8"
        );
    }

    #[lunatic_test::test]
    fn test_https_get_err() {
        let client = Client::new();
        assert!(client
            .request(Request::builder(Method::GET, "https://example-does-not-exst.com".parse().unwrap()).build())
            .is_err());
    }

    #[lunatic_test::test]
    fn test_http_get_not_found() {
        let client = Client::new();
        let response = client.request(
            Request::builder(
                Method::GET,
                "http://example.com/not_existing".parse().unwrap(),
            )
            .build(),
        ).unwrap();
        assert_eq!(response.status(), Status::NOT_FOUND);
    }

    #[lunatic_test::test]
    fn test_file_get_error() {
        let client = Client::new();
        assert!(client
            .request(
                Request::builder(
                    Method::GET,
                    "file://example.com/not_existing".parse().unwrap(),
                )
                .build(),
            )
            .is_err());
    }

    #[lunatic_test::test]
    fn test_redirection() {
        let mut client = Client::new();
        client.set_redirection_limit(5);
        let response = client.request(
            Request::builder(Method::GET, "http://wikipedia.org".parse().unwrap()).build(),
        ).unwrap();
        assert_eq!(response.status(), Status::OK);
    }
}
