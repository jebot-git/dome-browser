// SPDX-License-Identifier: MIT
// Shared BDF discovery and integer enlargement. The bit replication follows
// Francesco149/bdf2x (Unlicense); see bdf2x/upstream.json for the pinned source.
#pragma once
#include <fontconfig/fontconfig.h>
#include <zlib.h>
#include <algorithm>
#include <chrono>
#include <cctype>
#include <cstdlib>
#include <cstdint>
#include <filesystem>
#include <fstream>
#include <map>
#include <set>
#include <sstream>
#include <stdexcept>
#include <string>
#include <vector>
namespace bdf_fonts {
inline std::string identity(const std::string &family, const std::string &style) {
  auto variant = family.find(" [");
  if (variant != std::string::npos && family.back() == ']')
    return family.substr(0, variant) + " / " + style + family.substr(variant);
  return family + " / " + style;
}
struct Face {
  std::string file, family, style;
  int pixels = 0;
  bool text = true;
  std::string id() const { return identity(family, style); }
};
inline std::string read(const std::string &file) {
  gzFile f = gzopen(file.c_str(), "rb");
  if (!f) throw std::runtime_error("Cannot read BDF: " + file);
  std::string data;
  char buffer[16384]; int n;
  while ((n = gzread(f, buffer, sizeof buffer)) > 0) {
    data.append(buffer, n);
    if (data.size() > 128 * 1024 * 1024) { gzclose(f); throw std::runtime_error("BDF exceeds 128 MiB"); }
  }
  gzclose(f);
  if (n < 0 || data.rfind("STARTFONT ", 0) != 0) throw std::runtime_error("Invalid BDF: " + file);
  return data;
}
inline std::string twice(const std::string &data, const std::string &family = "") {
  std::istringstream input(data); std::ostringstream output;
  std::string line; bool bitmap = false;
  bool has_family = data.find("\nFAMILY_NAME ") != std::string::npos;
  bool has_properties = data.find("\nSTARTPROPERTIES ") != std::string::npos;
  const std::set<std::string> metrics = {"PIXEL_SIZE", "POINT_SIZE", "CAP_HEIGHT", "X_HEIGHT", "QUAD_WIDTH", "FONT_DESCENT", "FONT_ASCENT", "DWIDTH", "DWIDTH1", "BBX", "FONTBOUNDINGBOX"};
  while (std::getline(input, line)) {
    if (!line.empty() && line.back() == '\r') line.pop_back();
    auto split = line.find_first_of(" \t");
    auto key = line.substr(0, split);
    auto value = split == std::string::npos ? std::string() : line.substr(split + 1);
    if (key == "ENDCHAR") bitmap = false;
    if (bitmap) {
      if (line.size() > 8192 || line.size() % 2) throw std::runtime_error("Invalid BDF bitmap row");
      static const char digits[] = "0123456789ABCDEF";
      std::string row;
      for (char c : line) {
        auto at = std::string("0123456789ABCDEF").find(char(std::toupper((unsigned char)c)));
        if (at == std::string::npos) throw std::runtime_error("Invalid BDF bitmap digit");
        unsigned bits = 0;
        for (int bit = 3; bit >= 0; --bit) bits = (bits << 2) | ((at & (1 << bit)) ? 3 : 0);
        row += digits[bits >> 4]; row += digits[bits & 15];
      }
      output << row << '\n' << row << '\n';
      continue;
    }
    if (key == "STARTPROPERTIES" && !has_family && !family.empty()) {
      output << "STARTPROPERTIES " << std::stoi(value) + 1 << "\nFAMILY_NAME \"Dome 2x " << family << "\"\n";
    } else if (key == "CHARS" && !has_properties && !family.empty()) {
      output << "STARTPROPERTIES 1\nFAMILY_NAME \"Dome 2x " << family << "\"\nENDPROPERTIES\n" << line << '\n';
    } else if (key == "FAMILY_NAME") {
      auto first = value.find('"'), last = value.rfind('"');
      if (first == std::string::npos || first == last) throw std::runtime_error("Invalid BDF family");
      output << "FAMILY_NAME \"Dome 2x " << value.substr(first + 1, last - first - 1) << "\"\n";
    } else if (key == "FONT") {
      // XLFD: foundry, family, weight, slant, width, style, pixels, points,
      // resolution x/y, spacing, average width, registry and encoding.
      std::vector<std::string> fields; std::istringstream name(value); std::string field;
      while (std::getline(name, field, '-')) fields.push_back(field);
      if (fields.size() != 15) { output << "FONT Dome2x-" << value << '\n'; continue; }
      fields[2] = "Dome 2x " + fields[2];
      for (int index : {7, 8, 12}) {
        if (!fields[index].empty() && std::all_of(fields[index].begin(), fields[index].end(), [](unsigned char c){return std::isdigit(c);}))
          fields[index] = std::to_string(std::stoll(fields[index]) * 2);
      }
      output << "FONT "; for (size_t i = 0; i < fields.size(); ++i) output << (i ? "-" : "") << fields[i]; output << '\n';
    } else if (metrics.count(key) || key == "SIZE") {
      output << key; std::istringstream numbers(value); long n; int index = 0;
      while (numbers >> n) {
        if (std::abs(n) > 65536) throw std::runtime_error("BDF metric out of range");
        output << ' ' << n * (key == "SIZE" && index ? 1 : 2); ++index;
      }
      output << '\n';
    } else output << line << '\n';
    if (key == "BITMAP") bitmap = true;
  }
  return output.str();
}
inline bool inspect(const std::string &file, Face &face) {
  if (file.size() >= 3 && file.substr(file.size()-3) == ".gz") {
    try {
      auto bytes = read(file); unsigned long long hash=14695981039346656037ull;
      for(unsigned char c:bytes){hash^=c;hash*=1099511628211ull;}
      const char *base=getenv("XDG_CACHE_HOME"),*home=getenv("HOME");
      auto directory=(std::filesystem::path(base ? base : home ? std::string(home)+"/.cache" : "/tmp")/"de-xr/bdf-unpacked");
      std::filesystem::create_directories(directory);auto path=directory/(std::to_string(hash)+".bdf");
      if(!std::filesystem::exists(path)){
        auto temp=path;temp+="."+std::to_string(std::chrono::steady_clock::now().time_since_epoch().count())+".tmp";
        {std::ofstream out(temp,std::ios::binary);out.write(bytes.data(),bytes.size());if(!out)return false;}
        std::filesystem::rename(temp,path);
      }
      return inspect(path.string(),face);
    } catch(const std::exception &){return false;}
  }
  int count = 0; auto p = FcFreeTypeQuery((const FcChar8 *)file.c_str(), 0, nullptr, &count);
  if (!p) return false;
  FcChar8 *format = nullptr, *family = nullptr, *style = nullptr; double pixels = 0;
  bool ok = FcPatternGetString(p, FC_FONTFORMAT, 0, &format) == FcResultMatch &&
    !FcStrCmp(format, (const FcChar8 *)"BDF") &&
    FcPatternGetString(p, FC_FAMILY, 0, &family) == FcResultMatch &&
    FcPatternGetDouble(p, FC_PIXEL_SIZE, 0, &pixels) == FcResultMatch && pixels >= 4 && pixels <= 128;
  if (ok) {
    FcPatternGetString(p, FC_STYLE, 0, &style);
    FcCharSet *charset = nullptr;
    bool text = FcPatternGetCharSet(p, FC_CHARSET, 0, &charset) == FcResultMatch && FcCharSetHasChar(charset, 65);
    face = {file, (const char *)family, style ? (const char *)style : "Regular", int(pixels + .5), text};
  }
  FcPatternDestroy(p); return ok;
}
inline std::vector<Face> discover(bool force = false) {
  static std::vector<Face> faces;
  static auto checked = std::chrono::steady_clock::time_point{};
  auto now = std::chrono::steady_clock::now();
  if (!force && now - checked < std::chrono::seconds(2)) return faces;
  checked = now;
  struct Metadata { std::filesystem::file_time_type modified; Face face; bool valid; };
  static std::map<std::string, Metadata> metadata;
  std::set<std::string> files, directories;
  if (auto list = FcConfigGetFontDirs(nullptr)) {
    while (auto p = FcStrListNext(list)) directories.emplace((const char *)p);
    FcStrListDone(list);
  }
  for (const char *dir : {"/usr/share/fonts", "/usr/local/share/fonts", "/usr/local/share/fonts/domed", "/usr/share/fonts/domed"}) directories.emplace(dir);
  if (auto home = getenv("HOME")) { directories.emplace(std::string(home) + "/.fonts"); directories.emplace(std::string(home) + "/.local/share/fonts"); }
  if (auto data = getenv("XDG_DATA_HOME")) directories.emplace(std::string(data) + "/fonts");
  if (auto extra = getenv("DE_XR_BDF_DIRECTORY")) directories.emplace(extra);
  auto cache_home = getenv("XDG_CACHE_HOME"); auto user_home = getenv("HOME");
  if (cache_home || user_home) directories.emplace((std::filesystem::path(cache_home ? cache_home : std::string(user_home)+"/.cache") / "de-xr/bundled/fonts").string());
  std::error_code location_error;
  auto executable = std::filesystem::read_symlink("/proc/self/exe", location_error);
  if (!location_error) {
    directories.emplace((executable.parent_path() / "fonts").string());
    directories.emplace((executable.parent_path() / "../share/fonts/domed").string());
  }
  auto source = std::filesystem::path(__FILE__).parent_path();
  directories.emplace((source / "../assets/fonts").string());
  directories.emplace((source / "../../dome-compositor/assets/fonts").string());
  for (int parent = 0; parent < 5 && source.has_parent_path(); ++parent) {
    directories.emplace((source / "dome-compositor/assets/fonts").string());
    source = source.parent_path();
  }
  auto objects = FcObjectSetBuild(FC_FILE, nullptr);
  auto filter = FcPatternCreate();
  FcPatternAddString(filter, FC_FONTFORMAT, (const FcChar8 *)"BDF");
  auto listed = FcFontList(nullptr, filter, objects);
  FcPatternDestroy(filter);
  if (listed) { for (int i = 0; i < listed->nfont; ++i) { FcChar8 *file; if (FcPatternGetString(listed->fonts[i], FC_FILE, 0, &file) == FcResultMatch) files.emplace((const char *)file); } FcFontSetDestroy(listed); }
  FcObjectSetDestroy(objects);
  // Fontconfig policies commonly hide BDFs. Query configured directories and
  // add verified BDFs as application fonts without changing the host policy.
  for (auto &directory : directories) {
    std::error_code error;
    auto index_path = std::filesystem::path(directory) / "fonts.index";
    auto index_time = std::filesystem::last_write_time(index_path, error);
    if (!error) {
      std::ifstream index(index_path); std::string line;
      while (std::getline(index, line)) {
        std::vector<std::string> columns; std::string column; std::istringstream row(line);
        while (std::getline(row, column, '\t')) columns.push_back(column);
        if (columns.size() != 6) continue;
        auto path = (std::filesystem::path(directory)/columns[0]).string();
        auto modified = std::filesystem::last_write_time(path, error);
        if (error || modified > index_time) { error.clear(); continue; }
        try {
          Face face{path, columns[1], columns[2], std::stoi(columns[3]), columns[4] == "1"};
          if (face.pixels >= 4 && face.pixels <= 128)
            metadata.insert_or_assign(path, Metadata{modified, face, true});
        } catch (const std::exception &) {}
      }
    }
    error.clear();
    for (std::filesystem::recursive_directory_iterator it(directory, std::filesystem::directory_options::skip_permission_denied, error), end; it != end; it.increment(error)) {
      if (error) { error.clear(); continue; }
      auto name = it->path().string(); auto suffix = name;
      std::transform(suffix.begin(),suffix.end(),suffix.begin(),[](unsigned char c){return std::tolower(c);});
      if (suffix.size() >= 4 && (suffix.substr(suffix.size()-4) == ".bdf" || (name.size() >= 7 && suffix.substr(suffix.size()-7) == ".bdf.gz"))) files.insert(name);
    }
  }
  faces.clear();
  for (auto &file : files) {
    // These derived faces have already been validated when generated. They
    // must not be reparsed or offered as user-selectable native faces.
    auto parent = std::filesystem::path(file).parent_path();
    auto cache_root = std::filesystem::path(cache_home ? cache_home : user_home ? std::string(user_home)+"/.cache" : "/tmp") / "de-xr";
    if (parent == cache_root/"bdf2x-v1" || parent == cache_root/"bdf-qt-v1" || parent == cache_root/"bdf-media-v1") continue;
    std::error_code error;
    auto modified = std::filesystem::last_write_time(file, error);
    if (error) continue;
    auto found = metadata.find(file);
    if (found == metadata.end() || found->second.modified != modified) {
      Face face; bool valid = inspect(file, face);
      found = metadata.insert_or_assign(file, Metadata{modified, face, valid}).first;

    }
    Face f = found->second.face;
    if (found->second.valid && f.family.rfind("Dome 2x ", 0) != 0 && f.family.rfind("Dome Qt BDF ", 0) != 0 && f.family.rfind("Dome Media BDF ", 0) != 0) {
      faces.push_back(f);
    }
  }
  return faces;
}
inline void register_font(const std::string &file) {
  static std::set<std::string> registered;
  std::error_code error;auto modified=std::filesystem::last_write_time(file,error);
  if(error)return;
  auto key=std::to_string((uintptr_t)FcConfigGetCurrent())+":"+file+":"+std::to_string(modified.time_since_epoch().count());
  if(registered.insert(key).second)FcConfigAppFontAddFile(nullptr,(const FcChar8 *)file.c_str());
}
inline Face scaled(const Face &face, int scale) {
  if (scale == 1) {
    register_font(face.file);
    return face;
  }
  if (scale != 2) throw std::runtime_error("Font scale must be 1x or 2x");
  static std::map<std::string,std::pair<std::filesystem::file_time_type,Face>> cached;
  auto modified=std::filesystem::last_write_time(face.file);auto prior=cached.find(face.file);
  if(prior!=cached.end() && prior->second.first==modified && std::filesystem::exists(prior->second.second.file)){register_font(prior->second.second.file);return prior->second.second;}
  auto bytes = read(face.file);
  // A content key also invalidates files replaced at the same path.
  unsigned long long hash = 14695981039346656037ull;
  for (unsigned char c : bytes) { hash ^= c; hash *= 1099511628211ull; }
  auto base = getenv("XDG_CACHE_HOME"); auto home = getenv("HOME");
  std::filesystem::path cache = base && *base ? base : home ? std::string(home) + "/.cache" : "/tmp";
  cache /= "de-xr/bdf2x-v1";
  std::filesystem::create_directories(cache);
  auto file = cache / (std::to_string(hash) + ".bdf");
  if (!std::filesystem::is_regular_file(file)) {
    auto temp = file; temp += "." + std::to_string((unsigned long long)std::chrono::steady_clock::now().time_since_epoch().count()) + ".tmp";
    { std::ofstream out(temp, std::ios::binary); auto data = twice(bytes, face.family); out.write(data.data(), data.size()); if (!out) throw std::runtime_error("Cannot cache scaled BDF"); }
    std::filesystem::rename(temp, file);
  }
  Face enlarged;
  if (!inspect(file.string(), enlarged) || enlarged.pixels != face.pixels * 2) throw std::runtime_error("Invalid enlarged BDF");
  register_font(file.string());
  cached[face.file]={modified,enlarged};
  return enlarged;
}
// Qt groups equal family names from different foundries and may choose another
// strike. Give the exact BDF file a private family identity; pixels stay intact.
inline Face qt_alias(const Face &face, const std::string &role = "qt") {
  static std::map<std::string,std::pair<std::filesystem::file_time_type,Face>> cached;
  auto modified=std::filesystem::last_write_time(face.file);auto key=role+":"+face.file;auto prior=cached.find(key);
  if(prior!=cached.end() && prior->second.first==modified && std::filesystem::exists(prior->second.second.file))return prior->second.second;
  auto bytes=read(face.file);unsigned long long hash=14695981039346656037ull;
  for(unsigned char c:bytes){hash^=c;hash*=1099511628211ull;}
  auto family=std::string(role == "media" ? "Dome Media BDF " : "Dome Qt BDF ")+std::to_string(hash);
  auto base=getenv("XDG_CACHE_HOME"),home=getenv("HOME");
  auto directory=std::filesystem::path(base ? base : home ? std::string(home)+"/.cache" : "/tmp")/("de-xr/bdf-"+role+"-v1");
  std::filesystem::create_directories(directory);auto path=directory/(std::to_string(hash)+".bdf");
  if(!std::filesystem::exists(path)){
    std::istringstream input(bytes);std::ostringstream out;std::string line;bool property=false;
    while(std::getline(input,line)){
      if(line.rfind("FAMILY_NAME ",0)==0){line="FAMILY_NAME \""+family+"\"";property=true;}
      out<<line<<'\n';
    }
    if(!property)throw std::runtime_error("BDF lacks family metadata");
    auto temp=path;temp+="."+std::to_string(std::chrono::steady_clock::now().time_since_epoch().count())+".tmp";
    {std::ofstream file(temp,std::ios::binary);file<<out.str();if(!file)throw std::runtime_error("Cannot cache Qt BDF identity");}
    std::filesystem::rename(temp,path);
  }
  Face selected;
  if(!inspect(path.string(),selected))throw std::runtime_error("Invalid Qt BDF identity");
  cached[key]={modified,selected};
  return selected;
}
inline Face resolve(const std::string &family, const std::string &style, int pixels, int scale = 1) {
  auto faces = discover();
  auto found = std::find_if(faces.begin(), faces.end(), [&](const Face &f) { return (style.empty() ? f.family == family : f.id() == family + " / " + style) && f.pixels == pixels; });
  if (found == faces.end()) throw std::runtime_error("BDF face/size is unavailable: " + family);
  return scaled(*found, scale);
}
inline std::string export_config() {
  static const std::string inherited = [] {
    std::string value = getenv("DE_XR_FONTCONFIG_BASE") ? getenv("DE_XR_FONTCONFIG_BASE") : getenv("FONTCONFIG_FILE") ? getenv("FONTCONFIG_FILE") : "/etc/fonts/fonts.conf";
    setenv("DE_XR_FONTCONFIG_BASE", value.c_str(), 1); return value;
  }();
  auto escape = [](const std::string &value) {
    std::string out; for(char c:value) { if(c=='&')out+="&amp;";else if(c=='<')out+="&lt;";else if(c=='>')out+="&gt;";else out+=c; } return out;
  };
  std::set<std::string> directories;
  for (const auto &face : discover()) directories.insert(std::filesystem::path(face.file).parent_path().string());
  const char *base = getenv("XDG_CACHE_HOME"), *home = getenv("HOME");
  std::filesystem::path cache = base && *base ? base : home ? std::string(home)+"/.cache" : "/tmp";
  cache /= "de-xr";
  std::filesystem::create_directories(cache/"fontconfig");
  std::filesystem::create_directories(cache/"bdf2x-v1");
  directories.insert((cache/"bdf2x-v1").string());
  std::filesystem::create_directories(cache/"bdf-media-v1");
  directories.insert((cache/"bdf-media-v1").string());
  std::string xml = "<?xml version=\"1.0\"?><!DOCTYPE fontconfig SYSTEM \"urn:fontconfig:fonts.dtd\"><fontconfig><include ignore_missing=\"yes\">"+escape(inherited)+"</include>";
  for (const auto &directory : directories) xml += "<dir>"+escape(directory)+"</dir>";
  xml += "<selectfont><acceptfont><pattern><patelt name=\"scalable\"><bool>true</bool></patelt></pattern><glob>*.bdf</glob><glob>*.bdf.gz</glob><glob>*.BDF</glob><glob>*.BDF.gz</glob><pattern><patelt name=\"fontformat\"><string>BDF</string></patelt></pattern></acceptfont></selectfont></fontconfig>";
  unsigned long long hash=14695981039346656037ull;for(unsigned char c:xml){hash^=c;hash*=1099511628211ull;}
  auto path=cache/"fontconfig"/(std::to_string(hash)+".conf");
  if(!std::filesystem::is_regular_file(path)){
    auto temp=path;temp+="."+std::to_string((unsigned long long)std::chrono::steady_clock::now().time_since_epoch().count())+".tmp";
    {std::ofstream out(temp);out<<xml;if(!out)throw std::runtime_error("Cannot export BDF Fontconfig");}
    std::filesystem::rename(temp,path);
  }
  return path.string();
}
} // namespace bdf_fonts
