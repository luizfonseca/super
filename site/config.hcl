# superproxy.dev, served by Super on Railway: the home page from its files, and the docs from
# docs.page. Railway's edge (and Cloudflare in front of it) handles HTTPS: Super gets plain HTTP.

lets_encrypt {
  enabled = false
}

server {
  http_address = "0.0.0.0:8080"
}

logging {
  level = "INFO"
}

# The home page
host "superproxy.dev" {
  tls = false

  headers {
    response {
      add = {
        "Strict-Transport-Security" = "max-age=31536000"
        "X-Content-Type-Options"    = "nosniff"
        "Referrer-Policy"           = "strict-origin-when-cross-origin"
      }
    }
  }

  location "/" {
    static_files {
      directory     = "/var/www/site"
      cache_control = "public, max-age=300"
    }
    compress {}
  }

  location "/fonts" {
    static_files {
      directory     = "/var/www/site/fonts"
      cache_control = "public, max-age=31536000, immutable"
    }
  }
}

host "www.superproxy.dev" {
  tls = false

  location "/" {
    redirect {
      to     = "https://superproxy.dev{uri}"
      status = 301
    }
  }
}

# The docs, from docs.page (its pages link to /luizfonseca/super/... and /_next/...: those go as
# they are, everything else under the docs' prefix)
host "docs.superproxy.dev" {
  tls = false

  # Requests come through Cloudflare, whose headers docs.page's Cloudflare refuses
  rule "no cloudflare headers" {
    and {
      path {
        prefix = "/"
      }
    }
    then {
      headers {
        request {
          remove = ["cf-connecting-ip", "cf-ray", "cf-visitor", "cf-ipcountry", "cdn-loop"]
        }
      }
    }
  }

  headers {
    response {
      remove = ["x-powered-by"]
    }
  }

  location "/" {
    rewrite {
      add_prefix = "/luizfonseca/super"
    }
    upstream "docs.page:443" {
      sni = "docs.page"
      headers {
        add = { "Host" = "docs.page" }
      }
    }
    # docs.page also has IPv6 addresses, unreachable from some networks
    retry {
      attempts = 3
      on       = ["connect"]
    }
  }

  location "/luizfonseca/super" {
    upstream "docs.page:443" {
      sni = "docs.page"
      headers {
        add = { "Host" = "docs.page" }
      }
    }
    retry {
      attempts = 3
      on       = ["connect"]
    }
  }

  location "/_next" {
    upstream "docs.page:443" {
      sni = "docs.page"
      headers {
        add = { "Host" = "docs.page" }
      }
    }
    retry {
      attempts = 3
      on       = ["connect"]
    }
  }

  location "/_docs.page" {
    upstream "docs.page:443" {
      sni = "docs.page"
      headers {
        add = { "Host" = "docs.page" }
      }
    }
    retry {
      attempts = 3
      on       = ["connect"]
    }
  }
}
