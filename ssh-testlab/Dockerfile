FROM alpine:3.20

RUN apk add --no-cache \
        openssh-server \
        openssh-client \
        shadow \
    && mkdir -p /var/run/sshd \
    && ssh-keygen -A

ARG TEST_USER=testuser
ARG TEST_PASSWORD=testpass

RUN useradd \
        --create-home \
        --shell /bin/sh \
        "${TEST_USER}" \
    && echo "${TEST_USER}:${TEST_PASSWORD}" | chpasswd \
    && mkdir -p "/home/${TEST_USER}/.ssh" \
    && chmod 700 "/home/${TEST_USER}/.ssh" \
    && chown -R "${TEST_USER}:${TEST_USER}" "/home/${TEST_USER}/.ssh"

COPY scripts/entrypoint.sh /entrypoint.sh
RUN chmod 755 /entrypoint.sh

EXPOSE 22

ENTRYPOINT ["/entrypoint.sh"]
