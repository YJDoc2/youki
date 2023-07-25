use std::collections::HashMap;
use std::num::ParseIntError;
use std::path::PathBuf;
use zbus::blocking::{Connection, ConnectionBuilder};
use zbus::zvariant::Value;

#[derive(thiserror::Error, Debug)]
pub enum SystemdClientError {
    #[error("dbus error: {0}")]
    DBus(#[from] zbus::Error),
    #[error("failed to start transient unit {unit_name}, parent is {parent}: {err}")]
    FailedTransient {
        err: zbus::Error,
        unit_name: String,
        parent: String,
    },
    #[error("failed to stop unit {unit_name}: {err}")]
    FailedStop { err: zbus::Error, unit_name: String },
    #[error("failed to set properties for unit {unit_name}: {err}")]
    FailedProperties { err: zbus::Error, unit_name: String },
    #[error("could not parse systemd version: {0}")]
    SystemdVersion(ParseIntError),
}
pub trait SystemdClient {
    fn is_system(&self) -> bool;

    fn transient_unit_exists(&self, unit_name: &str) -> bool;

    fn start_transient_unit(
        &self,
        container_name: &str,
        pid: u32,
        parent: &str,
        unit_name: &str,
    ) -> Result<(), SystemdClientError>;

    fn stop_transient_unit(&self, unit_name: &str) -> Result<(), SystemdClientError>;

    fn set_unit_properties(
        &self,
        unit_name: &str,
        properties: &HashMap<&str, Value>,
    ) -> Result<(), SystemdClientError>;

    fn systemd_version(&self) -> Result<u32, SystemdClientError>;

    fn control_cgroup_root(&self) -> Result<PathBuf, SystemdClientError>;
}

/// Client is a wrapper providing higher level API and abatraction around dbus.
/// For more information see https://www.freedesktop.org/wiki/Software/systemd/dbus/
pub struct Client {
    conn: Connection,
    system: bool,
}

impl Client {
    /// Uses the system bus to communicate with systemd
    pub fn new_system() -> Result<Self, zbus::Error> {
        let conn = Connection::system()?;
        Ok(Client { conn, system: true })
    }

    /// Uses the session bus to communicate with systemd
    #[allow(unused)]
    pub fn new_session() -> Result<Self, zbus::Error> {
        let conn = Connection::session()?;
        Ok(Client {
            conn,
            system: false,
        })
    }

    pub fn new_session_with_uid(uid: u32) -> Result<Self, zbus::Error> {
        let conn = ConnectionBuilder::address("unix:path=/run/user/1000/bus")?
            .auth_mechanisms(&[zbus::AuthMechanism::ExternalUid(uid as i32)])
            .build()?;
        Ok(Client {
            conn,
            system: false,
        })
    }

    // fn create_proxy(&self) -> Proxy<&Connection> {
    //     self.conn.with_proxy(
    //         "org.freedesktop.systemd1",
    //         "/org/freedesktop/systemd1",
    //         Duration::from_millis(5000),
    //     )
    // }
}

impl SystemdClient for Client {
    fn is_system(&self) -> bool {
        self.system
    }

    fn transient_unit_exists(&self, unit_name: &str) -> bool {
        // let proxy = self.create_proxy();
        self.conn
            .call_method(
                Some("org.freedesktop.systemd1"),
                "/org/freedesktop/systemd1",
                Some("org.freedesktop.systemd1.Manager"),
                "GetUnit",
                &(unit_name.to_string(),),
            )
            .is_ok()
        // proxy.get_unit(unit_name).is_ok()
    }

    /// start_transient_unit is a higher level API for starting a unit
    /// for a specific container under systemd.
    /// See https://www.freedesktop.org/wiki/Software/systemd/dbus for more details.
    fn start_transient_unit(
        &self,
        container_name: &str,
        pid: u32,
        parent: &str,
        unit_name: &str,
    ) -> Result<(), SystemdClientError> {
        // To view and introspect the methods under the 'org.freedesktop.systemd1' destination
        // and object path under it use the following command:
        // `gdbus introspect --system --dest org.freedesktop.systemd1 --object-path /org/freedesktop/systemd1`

        // To align with runc, youki will always add the following properties to its container units:
        // - CPUAccounting=true
        // - IOAccounting=true (BlockIOAccounting for cgroup v1)
        // - MemoryAccounting=true
        // - TasksAccounting=true
        // see https://github.com/opencontainers/runc/blob/6023d635d725a74c6eaa11ab7f3c870c073badd2/docs/systemd.md#systemd-cgroup-driver
        // for more details.
        let mut properties: Vec<(&str, Value)> = Vec::with_capacity(6);
        properties.push((
            "Description",
            Value::Str(zbus::zvariant::Str::from(format!(
                "youki container {container_name}"
            ))),
        ));

        // if we create a slice, the parent is defined via a Wants=
        // otherwise, we use Slice=
        if unit_name.ends_with("slice") {
            properties.push((
                "Wants",
                Value::Str(zbus::zvariant::Str::from(parent.to_owned())),
            ));
        } else {
            properties.push((
                "Slice",
                Value::Str(zbus::zvariant::Str::from(parent.to_owned())),
            ));
            properties.push(("Delegate", Value::Bool(true)));
        }

        properties.push(("MemoryAccounting", Value::Bool(true)));
        properties.push(("CPUAccounting", Value::Bool(true)));
        properties.push(("IOAccounting", Value::Bool(true)));
        properties.push(("TasksAccounting", Value::Bool(true)));

        properties.push(("DefaultDependencies", Value::Bool(false)));
        properties.push(("PIDs", Value::Array(zbus::zvariant::Array::from(vec![pid]))));

        tracing::debug!("Starting transient unit: {:?}", properties);
        let aux: Vec<(&str, Vec<(&str, Value)>)> = vec![];
        self.conn
            .call_method(
                Some("org.freedesktop.systemd1"),
                "/org/freedesktop/systemd1",
                Some("org.freedesktop.systemd1.Manager"),
                "StartTransientUnit",
                &(
                    unit_name.to_string(),
                    "replace".to_string(),
                    properties,
                    aux,
                ),
            )
            .map_err(|err| SystemdClientError::FailedTransient {
                err,
                unit_name: unit_name.into(),
                parent: parent.into(),
            })?;
        Ok(())
    }

    fn stop_transient_unit(&self, unit_name: &str) -> Result<(), SystemdClientError> {
        // let proxy = self.create_proxy();
        self.conn
            .call_method(
                Some("org.freedesktop.systemd1"),
                "/org/freedesktop/systemd1",
                Some("org.freedesktop.systemd1.Manager"),
                "StopUnit",
                &(unit_name.to_string(), "replace".to_string()),
            )
            .map_err(|err| SystemdClientError::FailedStop {
                err,
                unit_name: unit_name.into(),
            })?;

        Ok(())
    }

    fn set_unit_properties(
        &self,
        unit_name: &str,
        properties: &HashMap<&str, Value>,
    ) -> Result<(), SystemdClientError> {
        let props: Vec<(_, _)> = properties
            .iter()
            .map(|(k, v)| ((*k).to_string(), v.clone()))
            .collect();
        self.conn
            .call_method(
                Some("org.freedesktop.systemd1"),
                "/org/freedesktop/systemd1",
                Some("org.freedesktop.systemd1.Manager"),
                "SetUnitProperties",
                &(unit_name.to_string(), true, props),
            )
            .map_err(|err| SystemdClientError::FailedProperties {
                err,
                unit_name: unit_name.into(),
            })?;

        Ok(())
    }

    fn systemd_version(&self) -> Result<u32, SystemdClientError> {
        let version = self
            .conn
            .call_method(
                Some("org.freedesktop.systemd1"),
                "/org/freedesktop/systemd1",
                Some("org.freedesktop.DBus.Properties"),
                "Get",
                &(
                    "org.freedesktop.systemd1.Manager".to_string(),
                    "Version".to_string(),
                ),
            )
            .and_then(|res| {
                let body: Value = res.body().unwrap();
                if let Value::Str(s) = body {
                    Ok(s.to_string())
                } else {
                    Err(zbus::Error::InvalidReply)
                }
            })?;

        let version = version
            .chars()
            .skip_while(|c| c.is_alphabetic())
            .take_while(|c| c.is_numeric())
            .collect::<String>()
            .parse::<u32>()
            .map_err(SystemdClientError::SystemdVersion)?;

        Ok(version)
    }

    fn control_cgroup_root(&self) -> Result<PathBuf, SystemdClientError> {
        // let proxy = self.create_proxy();
        let cgroup_root: String = self
            .conn
            .call_method(
                Some("org.freedesktop.systemd1"),
                "/org/freedesktop/systemd1",
                Some("org.freedesktop.DBus.Properties"),
                "Get",
                &(
                    "org.freedesktop.systemd1.Manager".to_string(),
                    "ControlGroup".to_string(),
                ),
            )
            .and_then(|res| {
                let body: Value = res.body().unwrap();
                if let Value::Str(s) = body {
                    Ok(s.to_string())
                } else {
                    Err(zbus::Error::InvalidReply)
                }
                // Ok(body.0.0)
            })?;
        // let cgroup_root = proxy.control_group()?;
        Ok(PathBuf::from(&cgroup_root))
        // Ok(PathBuf::from(""))
    }
}
