# Super, as the conformance suites test it: HTTPS (HTTP/2) and HTTP/3 on `localhost`, every
# request answered by Super itself. Its addresses and certificate come from `run.sh`.

lets_encrypt {
  enabled = false
}

logging {
  level               = "WARN"
  access_logs_enabled = false
}

server {
  http3 = true
}

host "localhost" {
  tls {
    certificate {
      pem = env("CONFORMANCE_CERT")
      key = env("CONFORMANCE_KEY")
    }
  }

  location "/" {
    respond {
      status       = 200
      body         = "Super, conformance: a body long enough for flow control to matter."
      content_type = "text/plain"
    }
  }
}
