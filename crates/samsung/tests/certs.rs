mod support;

use std::sync::{Arc, Mutex};

use retv_bridge_samsung::certs::{
    create, multipart, valid_duid, CertApi, CertStore, DEVELOPER_CA, DISTRIBUTOR_CA,
};

#[test]
fn multipart_body_is_exact_and_sized_once() {
    let b = multipart(
        "B",
        &[("user_id", "u1"), ("platform", "VD")],
        ("csr", "a.csr", "PEM"),
    );
    let text = String::from_utf8(b.to_vec()).unwrap();
    assert_eq!(
        text,
        "--B\r\nContent-Disposition: form-data; name=\"user_id\"\r\n\r\nu1\r\n\
--B\r\nContent-Disposition: form-data; name=\"platform\"\r\n\r\nVD\r\n\
--B\r\nContent-Disposition: form-data; name=\"csr\"; filename=\"a.csr\"\r\nContent-Type: application/octet-stream\r\n\r\nPEM\r\n--B--\r\n"
    );
    assert_eq!(b.capacity(), b.len(), "one allocation, no regrowth copies");
}
use retv_bridge_samsung::signin::parse_callback;
use retv_bridge_samsung::SamsungError;
use support::{http_server, SeenRequest};

/// Samsung's field name, and a fake value for it.
const AT: &str = "access_token";
const VALUE: &str = "fake-sign-in-123";
const LEAF_A: &str = "-----BEGIN CERTIFICATE-----\nQUFBQQ==\n-----END CERTIFICATE-----\n";
const LEAF_D: &str = "-----BEGIN CERTIFICATE-----\nRERERA==\n-----END CERTIFICATE-----\n";
const PROFILE: &str = "<?xml version=\"1.0\"?><profile name=\"x\"/>";

fn sign_in() -> retv_bridge_samsung::signin::SignIn {
    parse_callback(
        &format!("{AT}={VALUE}&userId=u42&inputEmailID=me%40example.com"),
        "",
    )
    .unwrap()
}

fn field(req: &SeenRequest, name: &str) -> Option<String> {
    let body = String::from_utf8_lossy(&req.body).to_string();
    let marker = format!("name=\"{name}\"");
    let at = body.find(&marker)?;
    let rest = &body[at..];
    let start = rest.find("\r\n\r\n")? + 4;
    let end = rest[start..].find("\r\n--")?;
    Some(rest[start..start + end].to_string())
}

#[tokio::test]
async fn create_against_fake_samsung() {
    let seen: Arc<Mutex<Vec<SeenRequest>>> = Arc::default();
    let seen2 = seen.clone();
    let addr = http_server(move |req| {
        seen2.lock().unwrap().push(req.clone());
        let ct = req
            .headers
            .iter()
            .find(|(k, _)| k == "content-type")
            .map(|(_, v)| v.clone())
            .unwrap_or_default();
        assert!(ct.starts_with("multipart/form-data; boundary="), "{ct}");
        match req.path.as_str() {
            "/apis/v3/authors" => (200, LEAF_A.to_string()),
            "/apis/v1/distributors" => (200, PROFILE.to_string()),
            "/apis/v3/distributors" => (200, LEAF_D.to_string()),
            p => panic!("unexpected {p}"),
        }
    })
    .await;
    let api = CertApi::new(format!("http://{addr}"));
    let cert = create(&api, &sign_in(), "2DCNLQ722RJQQ", 1_762_000_000_000)
        .await
        .unwrap();

    let reqs = seen.lock().unwrap().clone();
    let paths: Vec<&str> = reqs.iter().map(|r| r.path.as_str()).collect();
    assert_eq!(
        paths,
        [
            "/apis/v3/authors",
            "/apis/v1/distributors",
            "/apis/v3/distributors"
        ]
    );
    for r in &reqs {
        assert_eq!(r.method, "POST");
        assert_eq!(field(r, AT).as_deref(), Some(VALUE));
        assert_eq!(field(r, "user_id").as_deref(), Some("u42"));
        assert_eq!(field(r, "platform").as_deref(), Some("VD"));
        let csr = field(r, "csr").unwrap();
        assert!(csr.starts_with("-----BEGIN CERTIFICATE REQUEST-----"));
    }
    assert!(String::from_utf8_lossy(&reqs[0].body).contains("filename=\"author.csr\""));
    for r in &reqs[1..] {
        assert_eq!(field(r, "privilege_level").as_deref(), Some("public"));
        assert_eq!(field(r, "developer_type").as_deref(), Some("Individual"));
        assert!(String::from_utf8_lossy(&r.body).contains("filename=\"distributor.csr\""));
    }

    assert_eq!(cert.duid, "2DCNLQ722RJQQ");
    assert_eq!(
        cert.author_chain,
        vec![LEAF_A.to_string(), DEVELOPER_CA.replace("\r\n", "\n")]
    );
    assert_eq!(
        cert.distributor_chain,
        vec![LEAF_D.to_string(), DISTRIBUTOR_CA.replace("\r\n", "\n")]
    );
    assert_eq!(cert.profile_xml, PROFILE);
    assert!(cert
        .author_key_pem
        .starts_with(concat!("-----BEGIN ", "PRIVATE KEY-----")));
    assert_ne!(cert.author_key_pem, cert.distributor_key_pem);
    let shown = format!("{cert:?}");
    assert!(!shown.contains(concat!("PRIVATE", " KEY")), "{shown}");
}

#[tokio::test]
async fn too_many_requests_and_samsung_errors() {
    let addr = http_server(|req| match req.path.as_str() {
        "/apis/v3/authors" => (403, String::new()),
        _ => (
            400,
            "{\"error\":{\"description\":\"Invalid DUID\"}}".to_string(),
        ),
    })
    .await;
    let api = CertApi::new(format!("http://{addr}"));
    let err = create(&api, &sign_in(), "2DCNLQ722RJQQ", 0)
        .await
        .unwrap_err();
    assert_eq!(err, SamsungError::TooManyRequests);
    let err = api.distributor(&sign_in(), "csr").await.unwrap_err();
    assert_eq!(err, SamsungError::Samsung("Invalid DUID".into()));
    assert!(!err.to_string().contains(VALUE));
}

#[test]
fn duid_shape() {
    assert!(valid_duid("2DCNLQ722RJQQ"));
    assert!(!valid_duid("a/../etc"));
    assert!(!valid_duid("ab"));
    assert!(!valid_duid(""));
}

#[tokio::test]
async fn store_round_trip_and_private() {
    let addr = http_server(|req| match req.path.as_str() {
        "/apis/v3/authors" => (200, LEAF_A.to_string()),
        "/apis/v1/distributors" => (200, PROFILE.to_string()),
        _ => (200, LEAF_D.to_string()),
    })
    .await;
    let cert = create(
        &CertApi::new(format!("http://{addr}")),
        &sign_in(),
        "2DCNLQ722RJQQ",
        5,
    )
    .await
    .unwrap();
    let tmp = tempfile::tempdir().unwrap();
    let store = CertStore::new(tmp.path().join("samsung"));
    assert!(store.load("2DCNLQ722RJQQ").is_none());
    store.save(&cert).unwrap();
    let back = store.load("2DCNLQ722RJQQ").unwrap();
    assert_eq!(back.author_key_pem, cert.author_key_pem);
    assert_eq!(back.created_ms, 5);
    assert!(store.load("OTHERDUID1").is_none());
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let dir = std::fs::metadata(tmp.path().join("samsung"))
            .unwrap()
            .permissions()
            .mode()
            & 0o777;
        assert_eq!(dir, 0o700);
        let file = std::fs::metadata(tmp.path().join("samsung/cert-2DCNLQ722RJQQ.json"))
            .unwrap()
            .permissions()
            .mode()
            & 0o777;
        assert_eq!(file, 0o600);
    }
    // Other private files go through the same door.
    store.write_private("before-x.json", b"{}").unwrap();
    assert_eq!(
        std::fs::read(tmp.path().join("samsung/before-x.json")).unwrap(),
        b"{}"
    );
    assert_eq!(store.read_private("before-x.json").unwrap(), b"{}");
}
