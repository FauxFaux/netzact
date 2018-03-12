mod diag;
mod tcp;

use std::mem;
use std::os::unix::io::RawFd;

use cast::u16;
use cast::u32;
use cast::usize;

use libc;
use libc::c_int;

use libc::NLM_F_DUMP;
use libc::NLM_F_REQUEST;

use nix;
use nix::sys::socket;
use nix::sys::socket::AddressFamily;
use nix::sys::socket::SockProtocol;
use nix::sys::socket::SockType;
use nix::sys::uio::IoVec;

pub use self::diag::InetDiagMsg;
use self::diag::InetDiagReqV2;
use errors::*;

const SOCK_DIAG_BY_FAMILY: u16 = 20;

pub struct NetlinkDiag {
    fd: RawFd,
}

impl NetlinkDiag {
    pub fn new() -> Result<NetlinkDiag> {
        Ok(NetlinkDiag {
            fd: nix::errno::Errno::result(unsafe {
                libc::socket(
                    AddressFamily::Netlink as c_int,
                    SockType::Datagram as c_int,
                    libc::NETLINK_INET_DIAG,
                )
            })? as RawFd,
            //fd: socket(AddressFamily::Netlink, SockType::Datagram, SockFlag::SOCK_CLOEXEC, SockProtocol::from(libc::NETLINK_INET_DIAG))
            //  .chain_err(|| "opening netlink")
        })
    }

    pub fn ask_ip(&mut self, family: AddressFamily, protocol: SockProtocol) -> Result<()> {
        const INET_DIAG_INFO: u8 = 2;

        let header = NetlinkMessageHeader {
            len: u32(NETLINK_HEADER_LEN + mem::size_of::<InetDiagReqV2>()).unwrap(),
            flags: u16(NLM_F_REQUEST | NLM_F_DUMP).unwrap(),
            message_type: SOCK_DIAG_BY_FAMILY,
            ..NetlinkMessageHeader::default()
        };

        let header: [u8; mem::size_of::<NetlinkMessageHeader>()] =
            unsafe { mem::transmute(header) };

        let conn_req = InetDiagReqV2 {
            family: family as u8,
            protocol: protocol as u8,
            states: !0,
            ext: (1 << (INET_DIAG_INFO - 1)),
            ..InetDiagReqV2::default()
        };

        let conn_req: [u8; mem::size_of::<InetDiagReqV2>()] = unsafe { mem::transmute(conn_req) };

        let empty_netlink_address = socket::SockAddr::Netlink(socket::NetlinkAddr::new(0, 0));

        let vecs = [IoVec::from_slice(&header), IoVec::from_slice(&conn_req)];

        socket::sendmsg(
            self.fd,
            &vecs,
            &[],
            socket::MsgFlags::empty(),
            Some(&empty_netlink_address),
        )?;
        Ok(())
    }

    pub fn receive_until_done(&mut self) -> Result<Recv> {
        let mut ret = Recv {
            fd: self.fd,
            buf: [0u8; 8 * 1024],
            valid_bytes: 0,
            ptr: 0,
        };

        ret.recv()?;

        Ok(ret)
    }
}

pub struct Recv {
    fd: RawFd,
    buf: [u8; 8 * 1024],
    valid_bytes: usize,
    ptr: usize,
}

impl Recv {
    fn recv(&mut self) -> Result<()> {
        self.valid_bytes = socket::recv(self.fd, &mut self.buf, socket::MsgFlags::empty())?;
        self.ptr = 0;
        Ok(())
    }

    fn ok(&self) -> bool {
        let remaining = self.remaining();

        if remaining < NETLINK_HEADER_LEN {
            return false;
        }
        let next_len = usize(self.header().len);

        // TODO: off-by-one?
        next_len >= NETLINK_HEADER_LEN && next_len <= remaining
    }

    #[inline]
    fn remaining(&self) -> usize {
        self.valid_bytes
            .checked_sub(self.ptr)
            .expect("can't be past end")
    }

    #[inline]
    fn header(&self) -> &NetlinkMessageHeader {
        assert!(self.remaining() >= NETLINK_HEADER_LEN);
        unsafe { &*(self.buf[self.ptr..].as_ptr() as *const _) }
    }

    fn advance(&mut self) {
        self.ptr += netlink_next_message_starts_at(self.header());
    }

    pub fn next(&mut self) -> Result<Option<&InetDiagMsg>> {
        loop {
            if !self.ok() {
                self.recv()?;
                ensure!(self.ok(), "invalid after read; impossible");
            }

            const NLMSG_INET_DIAG: c_int = 20;

            let nl_header = *self.header();

            match nl_header.message_type as c_int {
                libc::NLMSG_DONE => return Ok(None),
                libc::NLMSG_ERROR => bail!("netlink error"),
                libc::NLMSG_OVERRUN => bail!("netlink overrun"),
                libc::NLMSG_NOOP => self.advance(),
                NLMSG_INET_DIAG => {
                    let main_len = NETLINK_HEADER_LEN + mem::size_of::<InetDiagMsg>();
                    ensure!(
                        self.remaining() >= main_len,
                        "not enough space in buf for an InetDiagMsg"
                    );
                    let data_starts_at = self.ptr + NETLINK_HEADER_LEN;
                    let val = unsafe { &*(self.buf[data_starts_at..].as_ptr() as *const _) };
                    println!("{}", usize(nl_header.len) - main_len);
                    self.advance();
                    return Ok(Some(val));
                }

                other => bail!("unsupported message type: {}", other),
            }
        }
    }
}

impl Drop for NetlinkDiag {
    fn drop(&mut self) {
        if 0 != unsafe { libc::close(self.fd) } {
            panic!("couldn't close fd {:?}", self.fd)
        }
    }
}

#[repr(C)]
#[derive(Copy, Clone, Default, Debug)]
pub struct NetlinkMessageHeader {
    /// Message length, including header.
    pub len: u32,
    pub message_type: u16,
    pub flags: u16,
    pub seq: u32,
    pub pid: u32,
}

pub const NETLINK_HEADER_LEN: usize = mem::size_of::<NetlinkMessageHeader>();

#[inline]
fn netlink_next_message_starts_at(header: &NetlinkMessageHeader) -> usize {
    netlink_msg_align(usize(header.len))
}

#[inline]
fn netlink_msg_align(len: usize) -> usize {
    const ALIGN_TO: usize = 4;

    ((len) + ALIGN_TO - 1) & !(ALIGN_TO - 1)
}
