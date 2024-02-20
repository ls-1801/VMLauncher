use crate::shell::run_shell_command_with_stdin;
use indoc::indoc;
use serde::Serialize;
use std::path::PathBuf;

#[derive(Debug, Serialize)]
struct Content {
    inline: String,
}

#[derive(Debug, Serialize)]
struct FlatcarStorageFileConfig {
    path: PathBuf,
    contents: Content,
}

#[derive(Debug, Serialize)]
struct FlatcarStorageConfig {
    files: Vec<FlatcarStorageFileConfig>,
}

#[derive(Debug, Serialize)]
struct FlatcarSystemdUnitConfig {
    name: String,
    enabled: bool,
    contents: String,
}

#[derive(Debug, Serialize)]
struct FlatcarSystemdConfig {
    units: Vec<FlatcarSystemdUnitConfig>,
}

#[derive(Debug, Serialize)]
struct FlatcarConfig {
    variant: String,
    version: String,
    systemd: FlatcarSystemdConfig,
    storage: FlatcarStorageConfig,
}

async fn run_butane(config: &FlatcarConfig) -> String {
    let data = serde_yaml::to_string(&config).unwrap();
    run_shell_command_with_stdin(
        "docker",
        vec!["run", "-i", "--rm", "quay.io/coreos/butane:latest"],
        data.as_bytes(),
    )
    .await
    .expect("could not run docker")
}

async fn prepare_launch() {}

#[test]
fn should_serialize_properly() {
    let config = FlatcarConfig {
        version: "1.0.0".to_string(),
        variant: "flatcar".to_string(),
        systemd: FlatcarSystemdConfig {
            units: vec![FlatcarSystemdUnitConfig {
                name: "nginx.service".to_string(),
                enabled: true,
                contents: indoc! {"
                    [Unit]
                    Description=NGINX service
                    [Service]
                    TimeoutStartSec=0
                    ExecStartPre=-/usr/bin/docker rm --force nginx1
                    ExecStart=/usr/bin/docker run --name worker -v /config:/config --pull always --log-driver=journald --net host 192.168.1.100:5000/nebulastream/nes-executable-image nesWorker --configPath=/config/workerConfig.yaml
                    ExecStop=/usr/bin/docker stop nginx1
                    Restart=always
                    RestartSec=5s
                    [Install]
                    WantedBy=multi-user.target
                "}.to_string(),
            }],
        },
        storage: FlatcarStorageConfig {
            files: vec![
                FlatcarStorageFileConfig {
                    path: PathBuf::from("/etc/systemd/network/00-eth0.network"),
                    contents: Content {
                        inline: indoc! {r#"
                          [Match]
                          Name=eth0

                          [Network]
                          DNS=1.1.1.1
                          Address=192.168.1.101/24
                          Gateway=192.168.1.100
                        "#}.to_string()
                    },
                },
                FlatcarStorageFileConfig {
                    path: PathBuf::from("/config/workerConfig.yaml"),
                    contents: Content {
                        inline: indoc! {r##"
                          workerId: 2
                          localWorkerIp: 192.168.1.101
                          coordinatorIp: 192.168.1.100
                          physicalSources:
                            - logicalSourceName: "bid"
                              physicalSourceName: "bid_phy"
                              type: TCP_SOURCE
                              configuration:
                                 socketHost: 192.168.1.100 #Set the host to connect to  (e.g. localhost)
                                 socketPort:  8091 # Set the port to connect to
                                 socketDomain:  AF_INET #Set the domain of the socket (e.g. AF_INET)
                                 socketType: SOCK_STREAM #Set the type of the socket (e.g. SOCK_STREAM)
                                 flushIntervalMS: 100 #Set the flushIntervalMS of the socket, if set to zero the buffer will only be flushed once full
                                 inputFormat:  CSV #Set the input format of the socket (e.g. JSON, CSV)
                                 decideMessageSize: TUPLE_SEPARATOR # Set the strategy for deciding the message size (e.g. TUPLE_SEPARATOR, USER_SPECIFIED_BUFFER_SIZE, BUFFER_SIZE_FROM_SOCKET)
                        "##}.to_string()
                    },
                },
                FlatcarStorageFileConfig {
                    path: PathBuf::from("/etc/docker/daemon.json"),
                    contents: Content {
                        inline: indoc! {r#"
                          {
                            "insecure-registries": ["192.168.1.100:5000"]
                          }
                        "#}.to_string()
                    },
                },
            ]
        },
    };

    let output = futures_lite::future::block_on(run_butane(&config));
    println!("{output}")
}
