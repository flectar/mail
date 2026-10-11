// Runs on the connected OHOS device; exercises the C ABI used by PDF preview.
#include "public/fpdfview.h"
#include "public/fpdf_edit.h"
#include "public/fpdf_save.h"
#include "public/fpdf_text.h"
#include <stddef.h>
#include <stdint.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>

typedef struct {
    FPDF_FILEWRITE writer;
    uint8_t* bytes;
    size_t length;
} MemoryWriter;

static int write_block(FPDF_FILEWRITE* writer, const void* data, unsigned long size) {
    MemoryWriter* memory = (MemoryWriter*)writer;
    if (size == 0) return 1;
    if (size > SIZE_MAX - memory->length) return 0;
    uint8_t* bytes = realloc(memory->bytes, memory->length + size);
    if (!bytes && size) return 0;
    memory->bytes = bytes;
    memcpy(bytes + memory->length, data, size);
    memory->length += size;
    return 1;
}

#define REQUIRE(value, name) do { if (!(value)) { \
    snprintf(report, capacity, "{\"status\":\"failed\",\"check\":\"%s\",\"pdfiumError\":%lu}", name, FPDF_GetLastError()); \
    goto cleanup; } } while (0)

int feihong_pdfium_probe(char* report, size_t capacity) {
    int result = 1;
    FPDF_DOCUMENT document = NULL;
    FPDF_PAGE page = NULL;
    FPDF_PAGEOBJECT rectangle = NULL, text = NULL;
    FPDF_TEXTPAGE text_page = NULL;
    FPDF_BITMAP bitmap = NULL;
    MemoryWriter memory = {{1, write_block}, NULL, 0};
    FPDF_LIBRARY_CONFIG config = {0};
    config.version = 2;
    FPDF_InitLibraryWithConfig(&config);
    document = FPDF_CreateNewDocument();
    REQUIRE(document, "create document");
    page = FPDFPage_New(document, 0, 120, 80);
    REQUIRE(page, "create page");
    rectangle = FPDFPageObj_CreateNewRect(10, 10, 70, 30);
    REQUIRE(rectangle, "create rectangle");
    REQUIRE(FPDFPageObj_SetFillColor(rectangle, 200, 10, 20, 255), "fill color");
    REQUIRE(FPDFPath_SetDrawMode(rectangle, FPDF_FILLMODE_WINDING, 0), "fill mode");
    FPDFPage_InsertObject(page, rectangle);
    rectangle = NULL; // Ownership transferred to the page.
    text = FPDFPageObj_NewTextObj(document, "Helvetica", 14);
    REQUIRE(text, "create text");
    const unsigned short message[] = {'F','e','i','h','o','n','g',' ','P','D','F',0};
    REQUIRE(FPDFText_SetText(text, message), "set text");
    FPDFPageObj_Transform(text, 1, 0, 0, 1, 10, 55);
    FPDFPage_InsertObject(page, text);
    text = NULL;
    REQUIRE(FPDFPage_GenerateContent(page), "generate page content");
    REQUIRE(FPDF_SaveAsCopy(document, &memory.writer, FPDF_NO_INCREMENTAL), "save document");
    FPDF_ClosePage(page);
    page = NULL;
    FPDF_CloseDocument(document);
    document = FPDF_LoadMemDocument64(memory.bytes, memory.length, NULL);
    REQUIRE(document, "open saved document");
    REQUIRE(FPDF_GetPageCount(document) == 1, "page count");
    page = FPDF_LoadPage(document, 0);
    REQUIRE(page, "load page");
    text_page = FPDFText_LoadPage(page);
    REQUIRE(text_page && FPDFText_CountChars(text_page) >= 11, "extract text");
    bitmap = FPDFBitmap_Create(120, 80, 0);
    REQUIRE(bitmap, "create bitmap");
    FPDFBitmap_FillRect(bitmap, 0, 0, 120, 80, 0xffffffff);
    FPDF_RenderPageBitmap(bitmap, page, 0, 0, 120, 80, 0, FPDF_ANNOT);
    const uint8_t* pixels = FPDFBitmap_GetBuffer(bitmap);
    int stride = FPDFBitmap_GetStride(bitmap), red = 0;
    REQUIRE(pixels && stride >= 480, "bitmap storage");
    for (int y = 0; y < 80; ++y) for (int x = 0; x < 120; ++x) {
        const uint8_t* p = pixels + y * stride + x * 4;
        if (p[2] > 180 && p[1] < 40 && p[0] < 50) ++red;
    }
    REQUIRE(red > 1000, "rendered rectangle pixels");
    snprintf(report, capacity,
        "{\"status\":\"passed\",\"checks\":[\"create\",\"save\",\"reload\",\"text\",\"render\"],"
        "\"pages\":1,\"textChars\":%d,\"redPixels\":%d,\"bytes\":%zu}",
        FPDFText_CountChars(text_page), red, memory.length);
    result = 0;
cleanup:
    if (bitmap) FPDFBitmap_Destroy(bitmap);
    if (text_page) FPDFText_ClosePage(text_page);
    if (page) FPDF_ClosePage(page);
    if (rectangle) FPDFPageObj_Destroy(rectangle);
    if (text) FPDFPageObj_Destroy(text);
    if (document) FPDF_CloseDocument(document);
    FPDF_DestroyLibrary();
    free(memory.bytes);
    return result;
}

#ifdef FEIHONG_PDFIUM_PROBE_STANDALONE
int main(void) {
    char report[512] = {0};
    int result = feihong_pdfium_probe(report, sizeof(report));
    puts(report);
    return result;
}
#endif
