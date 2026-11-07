import CoreText
import Foundation
var out = Set<String>()
let c = CTFontCollectionCreateFromAvailableFonts(nil)
if let ds = CTFontCollectionCreateMatchingFontDescriptors(c) as? [CTFontDescriptor] {
  for d in ds {
    for k in [kCTFontNameAttribute, kCTFontDisplayNameAttribute, kCTFontFamilyNameAttribute] {
      if let s = CTFontDescriptorCopyAttribute(d, k) as? String { out.insert(s) }
    }
  }
}
if let ps = CTFontManagerCopyAvailablePostScriptNames() as? [String] { for s in ps { out.insert(s) } }
if let fams = CTFontManagerCopyAvailableFontFamilyNames() as? [String] { for s in fams { out.insert(s) } }
for s in out.sorted() { print(s) }
