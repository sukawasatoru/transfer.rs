/*
 * Copyright 2019 sukawasatoru
 *
 * Licensed under the Apache License, Version 2.0 (the "License");
 * you may not use this file except in compliance with the License.
 * You may obtain a copy of the License at
 *
 *     http://www.apache.org/licenses/LICENSE-2.0
 *
 * Unless required by applicable law or agreed to in writing, software
 * distributed under the License is distributed on an "AS IS" BASIS,
 * WITHOUT WARRANTIES OR CONDITIONS OF ANY KIND, either express or implied.
 * See the License for the specific language governing permissions and
 * limitations under the License.
 */

use failure::Fail;
pub use failure::Fallible;

#[derive(Fail, Debug)]
#[fail(display = "Option error")]
pub struct OptionError;

pub trait OkOrErr<T> {
    fn ok_or_err(self) -> Fallible<T>;
}

impl<T> OkOrErr<T> for Option<T> {
    fn ok_or_err(self) -> Fallible<T> {
        self.ok_or_else(|| OptionError.into())
    }
}
