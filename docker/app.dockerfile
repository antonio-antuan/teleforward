ARG base_image_name=antonioantuan/teleforward-base
ARG base_image_version=latest

FROM ${base_image_name}:${base_image_version}


COPY . /usr/src/myapp
WORKDIR /usr/src/myapp
RUN cargo build --release && \
    cp target/release/teleforward /opt/teleforward && \
    rm -rf /usr/src/myapp


ENTRYPOINT ["/opt/teleforward"]
