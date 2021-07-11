use super::*;
use bytes::Buf;

const REQUEST_TYPE: u8 = 0b00100001;
const DFU_GETSTATUS: u8 = 3;
const DFU_CLRSTATUS: u8 = 4;

pub(crate) type PollTimeout = u64;
pub(crate) type Index = u8;
// TODO make a struct?
pub(crate) type GetStatusMessage = (Status, PollTimeout, State, Index);

/// Command that queries the status of the device.
#[must_use]
pub struct GetStatus<'dfu, IO: DfuIo, T: ChainedCommand<Arg = GetStatusMessage>> {
    pub(crate) dfu: &'dfu DfuSansIo<IO>,
    pub(crate) chained_command: T,
}

impl<'dfu, IO: DfuIo, T: ChainedCommand<Arg = GetStatusMessage>> GetStatus<'dfu, IO, T> {
    /// Query the status of the device.
    pub fn get_status(self, buffer: &mut [u8]) -> Result<(GetStatusRecv<T>, IO::Read), IO::Error> {
        debug_assert!(buffer.len() >= 6);
        let next = GetStatusRecv {
            chained_command: self.chained_command,
        };
        let res = self
            .dfu
            .io
            .read_control(REQUEST_TYPE, DFU_GETSTATUS, 0, buffer)?;
        Ok((next, res))
    }
}

/// Command that receives the status of the device and chain it into another command.
#[must_use]
pub struct GetStatusRecv<T: ChainedCommand<Arg = GetStatusMessage>> {
    chained_command: T,
}

impl<T: ChainedCommand<Arg = GetStatusMessage>> GetStatusRecv<T> {
    /// Receives the status of the device and chains the current command into another command.
    pub fn chain(self, mut bytes: &[u8]) -> Result<T::Into, Error> {
        if bytes.len() < 6 {
            return Err(Error::ResponseTooShort {
                got: bytes.len(),
                expected: 6,
            });
        }

        let status = match bytes.get_u8() {
            0x00 => Status::Ok,
            0x01 => Status::ErrTarget,
            0x02 => Status::ErrFile,
            0x03 => Status::ErrWrite,
            0x04 => Status::ErrErase,
            0x05 => Status::ErrCheckErased,
            0x06 => Status::ErrProg,
            0x07 => Status::ErrVerify,
            0x08 => Status::ErrAddress,
            0x09 => Status::ErrNotdone,
            0x0a => Status::ErrFirmware,
            0x0b => Status::ErrVendor,
            0x0c => Status::ErrUsbr,
            0x0d => Status::ErrPor,
            0x0e => Status::ErrUnknown,
            0x0f => Status::ErrStalledpkt,
            other => Status::Other(other),
        };
        let poll_timeout = bytes.get_uint_le(3);
        let state = match bytes.get_u8() {
            0 => State::AppIdle,
            1 => State::AppDetach,
            2 => State::DfuIdle,
            3 => State::DfuUnloadSync,
            4 => State::DfuDnbusy,
            5 => State::DfuDnloadIdle,
            6 => State::DfuManifestSync,
            7 => State::DfuManifest,
            8 => State::DfuManifestWaitReset,
            9 => State::DfuUploadIdle,
            10 => State::DfuError,
            other => State::Other(other),
        };
        let i_string = bytes.get_u8();

        status.raise_error()?;
        state.raise_error()?;

        Ok(self
            .chained_command
            .chain((status, poll_timeout, state, i_string)))
    }
}

/// Command that clears the status of the device.
#[must_use]
pub struct ClearStatus<'dfu, IO: DfuIo, T> {
    pub(crate) dfu: &'dfu DfuSansIo<IO>,
    pub(crate) chained_command: T,
}

impl<'dfu, IO: DfuIo, T> ClearStatus<'dfu, IO, T> {
    /// Clear the status of the device.
    pub fn clear(self) -> Result<(T, IO::Write), IO::Error> {
        let res = self
            .dfu
            .io
            .write_control(REQUEST_TYPE, DFU_CLRSTATUS, 0, &[])?;
        let next = self.chained_command;

        Ok((next, res))
    }
}

// TODO constructor
/// Command that waits for the device to enter a state.
#[must_use]
pub struct WaitState<'dfu, IO: DfuIo, T> {
    pub(crate) dfu: &'dfu DfuSansIo<IO>,
    pub(crate) state: State,
    pub(crate) chained_command: T,
    pub(crate) end: bool,
    pub(crate) poll_timeout: PollTimeout,
    pub(crate) in_manifest: bool,
}

/// Step to take after waiting for a state.
pub enum Step<'dfu, IO: DfuIo, T> {
    /// The state has been reached, the loop can be break.
    Break(T),
    /// The state has not been reached and the status of the device must be queried.
    Wait(GetStatus<'dfu, IO, WaitState<'dfu, IO, T>>, PollTimeout),
    /// The device is in manifest state and might require a USB reset.
    ManifestWaitReset(Option<reset::UsbReset<'dfu, IO, ()>>),
}

impl<'dfu, IO: DfuIo, T> WaitState<'dfu, IO, T> {
    /// Returns the next command after waiting for a state.
    pub fn next(self) -> Step<'dfu, IO, T> {
        let func_desc = self.dfu.io.functional_descriptor();

        if self.end {
            Step::Break(self.chained_command)
        } else if self.in_manifest && !func_desc.manifestation_tolerant {
            Step::ManifestWaitReset((!func_desc.will_detach).then(|| reset::UsbReset {
                dfu: self.dfu,
                chained_command: (),
            }))
        } else {
            let poll_timeout = self.poll_timeout;

            Step::Wait(
                GetStatus {
                    dfu: self.dfu,
                    chained_command: self,
                },
                poll_timeout,
            )
        }
    }
}

impl<'dfu, IO: DfuIo, T> ChainedCommand for WaitState<'dfu, IO, T> {
    type Arg = GetStatusMessage;
    type Into = Self;

    fn chain(self, (_status, poll_timeout, state, _index): Self::Arg) -> Self::Into {
        WaitState {
            dfu: self.dfu,
            chained_command: self.chained_command,
            state: self.state,
            end: state == self.state,
            poll_timeout,
            in_manifest: state == State::DfuManifest,
        }
    }
}
