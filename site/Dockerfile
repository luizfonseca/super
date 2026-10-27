# superproxy.dev: Super, with the home page and its configuration
FROM luizfonseca/super:latest
COPY config.hcl /etc/proksi/config.hcl
COPY public /var/www/site
EXPOSE 8080
CMD ["-c", "/etc/proksi"]
