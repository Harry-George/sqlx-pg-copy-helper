//! the ipnetwork type doesn't support to sql, sqlx doesn't support cidr, so we have this helper
//! which converts one to the other and then provides [`ToSql`] implementation for
//! it

use bytes::BytesMut;
use cidr::{IpCidr, IpInet, Ipv4Cidr, Ipv4Inet, Ipv6Cidr, Ipv6Inet};
use ipnetwork::IpNetwork;
use tokio_postgres::types::{IsNull, ToSql, Type, WrongType, accepts, to_sql_checked};

/// Newtype that wraps [`IpNetwork`] and serialises it as a `PostgreSQL` `CIDR` value.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct IpNetworkCidr(pub IpNetwork);

impl ToSql for IpNetworkCidr {
    fn to_sql(
        &self,
        ty: &Type,
        out: &mut BytesMut,
    ) -> Result<IsNull, Box<dyn std::error::Error + Sync + Send>> {
        match *ty {
            Type::INET => {
                let inet = match self.0 {
                    IpNetwork::V4(n) => IpInet::V4(Ipv4Inet::new(n.network(), n.prefix())?),
                    IpNetwork::V6(n) => IpInet::V6(Ipv6Inet::new(n.network(), n.prefix())?),
                };
                inet.to_sql(ty, out)
            }
            Type::CIDR => {
                let cidr = match self.0 {
                    IpNetwork::V4(n) => IpCidr::V4(Ipv4Cidr::new(n.network(), n.prefix())?),
                    IpNetwork::V6(n) => IpCidr::V6(Ipv6Cidr::new(n.network(), n.prefix())?),
                };
                cidr.to_sql(ty, out)
            }
            _ => Err(Box::new(WrongType::new::<IpNetworkCidr>(ty.clone()))),
        }
    }

    accepts!(INET, CIDR);

    to_sql_checked!();
}
