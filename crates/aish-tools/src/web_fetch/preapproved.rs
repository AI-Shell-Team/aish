/// Domains that can be fetched without an extra WebFetch confirmation.
///
/// This list is intentionally scoped to WebFetch GET requests. It should not be
/// reused as a general network allowlist for shell commands or sandbox egress.
const PREAPPROVED_HOSTS: &[&str] = &[
    "platform.claude.com",
    "code.claude.com",
    "modelcontextprotocol.io",
    "github.com/anthropics",
    "agentskills.io",
    "docs.python.org",
    "en.cppreference.com",
    "docs.oracle.com",
    "learn.microsoft.com",
    "developer.mozilla.org",
    "go.dev",
    "pkg.go.dev",
    "www.php.net",
    "docs.swift.org",
    "kotlinlang.org",
    "ruby-doc.org",
    "doc.rust-lang.org",
    "www.typescriptlang.org",
    "react.dev",
    "angular.io",
    "vuejs.org",
    "nextjs.org",
    "expressjs.com",
    "nodejs.org",
    "bun.sh",
    "jquery.com",
    "getbootstrap.com",
    "tailwindcss.com",
    "d3js.org",
    "threejs.org",
    "redux.js.org",
    "webpack.js.org",
    "jestjs.io",
    "reactrouter.com",
    "docs.djangoproject.com",
    "flask.palletsprojects.com",
    "fastapi.tiangolo.com",
    "pandas.pydata.org",
    "numpy.org",
    "www.tensorflow.org",
    "pytorch.org",
    "scikit-learn.org",
    "matplotlib.org",
    "requests.readthedocs.io",
    "jupyter.org",
    "laravel.com",
    "symfony.com",
    "wordpress.org",
    "docs.spring.io",
    "hibernate.org",
    "tomcat.apache.org",
    "gradle.org",
    "maven.apache.org",
    "asp.net",
    "dotnet.microsoft.com",
    "nuget.org",
    "blazor.net",
    "reactnative.dev",
    "docs.flutter.dev",
    "developer.apple.com",
    "developer.android.com",
    "keras.io",
    "spark.apache.org",
    "huggingface.co",
    "www.kaggle.com",
    "www.mongodb.com",
    "redis.io",
    "www.postgresql.org",
    "dev.mysql.com",
    "www.sqlite.org",
    "graphql.org",
    "prisma.io",
    "docs.aws.amazon.com",
    "cloud.google.com",
    "kubernetes.io",
    "www.docker.com",
    "www.terraform.io",
    "www.ansible.com",
    "vercel.com/docs",
    "docs.netlify.com",
    "devcenter.heroku.com",
    "cypress.io",
    "selenium.dev",
    "docs.unity.com",
    "docs.unrealengine.com",
    "git-scm.com",
    "nginx.org",
    "httpd.apache.org",
];

pub(crate) fn is_preapproved_host(hostname: &str, pathname: &str) -> bool {
    for entry in PREAPPROVED_HOSTS {
        if let Some((host, prefix)) = entry.split_once('/') {
            if hostname == host {
                let prefix = format!("/{}", prefix);
                if pathname == prefix || pathname.starts_with(&(prefix + "/")) {
                    return true;
                }
            }
        }
        if hostname == *entry {
            return true;
        }
    }
    false
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn preapproved_host_supports_path_prefix_boundary() {
        assert!(is_preapproved_host("github.com", "/anthropics/claude-code"));
        assert!(!is_preapproved_host(
            "github.com",
            "/anthropics-evil/project"
        ));
        assert!(is_preapproved_host("doc.rust-lang.org", "/book/"));
    }
}
